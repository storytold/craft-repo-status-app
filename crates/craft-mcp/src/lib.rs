//! A read-only Model Context Protocol server over Craft Status's cache, so
//! an agent can read what the window shows: the Repos table, one repo, the
//! People tab's Work rows, the My PRs tab and one pull request's record.
//!
//! The server speaks JSON-RPC 2.0 over stdio, one message or batch per line,
//! and answers `initialize`, `ping`, `tools/list` and `tools/call`
//! (<https://modelcontextprotocol.io/specification/2025-11-25>). It reads
//! the app's files on each call ([`tools`]), never sends a request to
//! GitHub and never writes a file.

pub mod tools;

use std::io::{self, BufRead, Write};

use serde_json::{json, Map, Value};

pub use tools::{Sources, Time};

/// The protocol versions the server speaks, newest first. `initialize`
/// answers with the client's version when it is one of these, and with the
/// newest otherwise.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// JSON-RPC error codes.
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;

const INSTRUCTIONS: &str = "Craft Status keeps a cache of the GitHub repos it watches. These tools read that cache: \
repos and repo for the Repos table, people for the People tab, my_prs for the My PRs tab, and pull_request for one \
record. The data is as old as the app's last fetch; each answer gives the times.";

/// The server: where it reads from, and its clock.
pub struct Server {
    sources: Sources,
    clock: Box<dyn Fn() -> Time>,
}

impl Server {
    pub fn new(sources: Sources) -> Server {
        Server::with_clock(sources, chrono::Utc::now)
    }

    /// A server whose answers are worked out at the times `clock` gives.
    pub fn with_clock(sources: Sources, clock: impl Fn() -> Time + 'static) -> Server {
        Server {
            sources,
            clock: Box::new(clock),
        }
    }

    /// Answers each line of `input` on a line of `output`, until `input`
    /// ends. Blank lines are skipped, and notifications get no answer.
    pub fn serve(&self, input: impl BufRead, mut output: impl Write) -> io::Result<()> {
        for line in input.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Some(answer) = self.handle_line(&line) {
                writeln!(output, "{answer}")?;
                output.flush()?;
            }
        }
        Ok(())
    }

    /// The answer to one line, or `None` for a notification or a response.
    pub fn handle_line(&self, line: &str) -> Option<String> {
        let answer = match serde_json::from_str::<Value>(line) {
            Ok(Value::Array(batch)) => self.handle_batch(&batch)?,
            Ok(message) => self.handle(&message)?,
            Err(e) => error(Value::Null, PARSE_ERROR, &format!("Parse error: {e}")),
        };
        Some(answer.to_string())
    }

    /// The answers to a JSON-RPC batch as one array, in the order of the
    /// messages that get one, or `None` when every message is a
    /// notification or a response. An empty batch gets a single error.
    pub fn handle_batch(&self, batch: &[Value]) -> Option<Value> {
        if batch.is_empty() {
            return Some(error(
                Value::Null,
                INVALID_REQUEST,
                "Invalid request: empty batch",
            ));
        }
        let answers: Vec<Value> = batch.iter().filter_map(|m| self.handle(m)).collect();
        (!answers.is_empty()).then_some(Value::Array(answers))
    }

    /// The answer to one message, or `None` for a notification or a
    /// response.
    pub fn handle(&self, message: &Value) -> Option<Value> {
        let Some(m) = message.as_object() else {
            return Some(error(
                Value::Null,
                INVALID_REQUEST,
                "Invalid request: not a JSON object",
            ));
        };
        let id = m.get("id").cloned();
        let Some(method) = m.get("method").and_then(Value::as_str) else {
            // A response from the client (`result` or `error`) needs no answer.
            if m.contains_key("result") || m.contains_key("error") {
                return None;
            }
            return Some(error(
                id.unwrap_or(Value::Null),
                INVALID_REQUEST,
                "Invalid request: no method",
            ));
        };
        if m.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return id.map(|id| {
                error(
                    id,
                    INVALID_REQUEST,
                    "Invalid request: jsonrpc must be \"2.0\"",
                )
            });
        }
        let id = id?;
        let empty = Map::new();
        let params = match m.get("params") {
            None | Some(Value::Null) => &empty,
            Some(Value::Object(p)) => p,
            Some(_) => return Some(error(id, INVALID_PARAMS, "params must be an object")),
        };
        Some(match method {
            "initialize" => self.initialize(id, params),
            "ping" => result(id, json!({})),
            "tools/list" => result(id, json!({"tools": tools::list()})),
            "tools/call" => self.call(id, params),
            _ => error(id, METHOD_NOT_FOUND, &format!("Method not found: {method}")),
        })
    }

    fn initialize(&self, id: Value, params: &Map<String, Value>) -> Value {
        let Some(asked) = params.get("protocolVersion").and_then(Value::as_str) else {
            return error(
                id,
                INVALID_PARAMS,
                "initialize needs params.protocolVersion",
            );
        };
        let version = PROTOCOL_VERSIONS
            .iter()
            .find(|v| **v == asked)
            .unwrap_or(&PROTOCOL_VERSIONS[0]);
        result(
            id,
            json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {
                    "name": "craft-mcp",
                    "title": "Craft Status",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "instructions": INSTRUCTIONS,
            }),
        )
    }

    fn call(&self, id: Value, params: &Map<String, Value>) -> Value {
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return error(id, INVALID_PARAMS, "tools/call needs params.name");
        };
        let empty = Map::new();
        let args = match params.get("arguments") {
            None | Some(Value::Null) => &empty,
            Some(Value::Object(a)) => a,
            Some(_) => return error(id, INVALID_PARAMS, "params.arguments must be an object"),
        };
        match tools::call(name, args, &self.sources, (self.clock)()) {
            None => error(id, INVALID_PARAMS, &format!("Unknown tool: {name}")),
            Some(Ok(answer)) => result(
                id,
                json!({
                    "content": [{"type": "text", "text": answer.to_string()}],
                    "structuredContent": answer,
                    "isError": false,
                }),
            ),
            Some(Err(message)) => result(
                id,
                json!({
                    "content": [{"type": "text", "text": message}],
                    "isError": true,
                }),
            ),
        }
    }
}

fn result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use craft_core::people::record::{Checks, MainChecks, PrCache, PrRecord, PrState};
    use craft_core::people::viewer;
    use craft_core::{Activity, Cache, RepoEntry, RepoStats, Snapshot};

    use super::*;

    fn t(day: u32, hour: u32) -> Time {
        format!("2026-10-{day:02}T{hour:02}:00:00Z")
            .parse()
            .unwrap()
    }

    /// The fixed time of every call.
    fn now() -> Time {
        t(18, 12)
    }

    fn record(number: u64, author: &str) -> PrRecord {
        PrRecord {
            id: format!("PR_{number}"),
            number,
            url: format!("https://github.com/example-org/alpha/pull/{number}"),
            title: format!("Change {number}"),
            author: Some(author.into()),
            created_at: Some(t(10, 0)),
            updated_at: Some(t(17, 0)),
            fetched_at: Some(t(18, 0)),
            checks: Checks {
                mergeable: "MERGEABLE".into(),
                rollup: Some("SUCCESS".into()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// A cache directory and a config with made-up repos and people, in a
    /// directory of its own named after `name`. `my_login` goes into the
    /// config's `[people]` table.
    fn fixture(name: &str, my_login: Option<&str>) -> Sources {
        let root = std::env::temp_dir().join(format!("craft-mcp-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let dir = root.join("cache");
        let config = root.join("config.toml");
        let mut text = String::from(
            "repos = [\"example-org/alpha\", \"https://github.com/example-org/beta\"]\n\
             github_token = \"not-a-real-token\"\n\
             [people]\nmaintainers = [\"maint-one\"]\n",
        );
        if let Some(l) = my_login {
            text.push_str(&format!("my_login = \"{l}\"\n"));
        }
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&config, text).unwrap();

        let stats = |urgency: f64, commits: u64| RepoStats {
            open_prs: 3,
            open_issues: 7,
            urgency,
            recent: [(
                "4h".to_string(),
                Activity {
                    commits,
                    ..Default::default()
                },
            )]
            .into(),
            latest_build: Some("v1.2.0".into()),
            latest_build_at: Some(t(15, 0)),
            ..Default::default()
        };
        let snap = Snapshot {
            repos: vec![
                RepoEntry {
                    fetched_at: Some(t(18, 11)),
                    stats: Some(stats(12.5, 5)),
                    icon: Some("data:image/png;base64,AAAA".into()),
                    ..RepoEntry::new("example-org/alpha")
                },
                RepoEntry {
                    fetched_at: Some(t(18, 9)),
                    stats: Some(stats(30.0, 2)),
                    error: Some("timed out".into()),
                    ..RepoEntry::new("example-org/beta")
                },
            ],
            rate_limit_remaining: Some(4200),
            ..Default::default()
        };
        Cache::at(dir.join("snapshot.json")).save(&snap).unwrap();

        let mut alpha = PrCache {
            main_checks: Some(MainChecks {
                oid: "main-head".into(),
                rollup: Some("SUCCESS".into()),
                failing: vec![],
            }),
            checked_at: Some(t(18, 10)),
            ..Default::default()
        };
        let mut failing = record(1, "Ann-Example");
        failing.checks.rollup = Some("FAILURE".into());
        failing.checks.failing = vec!["build (macos)".into()];
        failing.checks.merge_state = "BLOCKED".into();
        let mut unread = record(2, "ann-example");
        unread.fetched_at = None;
        let other = record(3, "bo-example");
        let mut merged = record(4, "ann-example");
        merged.state = PrState::Merged;
        merged.merged_at = Some(t(17, 0));
        merged.merged_by = Some("maint-one".into());
        for r in [failing, unread, other, merged] {
            alpha.records.insert(r.number, r);
        }
        alpha.save(&dir, "example-org/alpha").unwrap();
        let mut beta = PrCache::default();
        let mut conflict = record(7, "ann-example");
        conflict.url = "https://github.com/example-org/beta/pull/7".into();
        conflict.checks.mergeable = "CONFLICTING".into();
        conflict.checks.conflict_since = Some(t(16, 0));
        beta.records.insert(7, conflict);
        beta.save(&dir, "example-org/beta").unwrap();
        Sources {
            dir,
            config: Some(config),
        }
    }

    fn server(src: &Sources) -> Server {
        Server::with_clock(src.clone(), now)
    }

    fn request(s: &Server, id: u64, method: &str, params: Value) -> Value {
        let line =
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
        let answer = s.handle_line(&line).expect("a request gets an answer");
        serde_json::from_str(&answer).unwrap()
    }

    /// The structured answer of a successful tool call.
    fn call(s: &Server, tool: &str, args: Value) -> Value {
        let a = request(s, 9, "tools/call", json!({"name": tool, "arguments": args}));
        assert_eq!(a["result"]["isError"], false, "{a}");
        let text = a["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(
            parsed, a["result"]["structuredContent"],
            "the text holds the same JSON"
        );
        parsed
    }

    /// The message of a tool error.
    fn call_error(s: &Server, tool: &str, args: Value) -> String {
        let a = request(s, 9, "tools/call", json!({"name": tool, "arguments": args}));
        assert_eq!(a["result"]["isError"], true, "{a}");
        a["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn cleanup(src: &Sources) {
        if let Some(root) = src.dir.parent() {
            std::fs::remove_dir_all(root).ok();
        }
    }

    #[test]
    fn initialize_ping_and_tools_list_answer_by_the_protocol() {
        let src = fixture("protocol", None);
        let s = server(&src);
        let a = request(
            &s,
            1,
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}),
        );
        assert_eq!(a["id"], 1);
        assert_eq!(a["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(a["result"]["serverInfo"]["name"], "craft-mcp");
        assert_eq!(a["result"]["capabilities"]["tools"]["listChanged"], false);
        let a = request(
            &s,
            2,
            "initialize",
            json!({"protocolVersion": "1999-01-01"}),
        );
        assert_eq!(
            a["result"]["protocolVersion"], PROTOCOL_VERSIONS[0],
            "an unknown version gets the newest"
        );
        assert_eq!(
            s.handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
            None,
            "a notification gets no answer"
        );
        assert_eq!(request(&s, 3, "ping", json!({}))["result"], json!({}));
        let a = request(&s, 4, "tools/list", Value::Null);
        let tools = a["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, tools::NAMES);
        for t in tools {
            assert_eq!(t["inputSchema"]["type"], "object", "{t}");
            assert_eq!(t["annotations"]["readOnlyHint"], true);
            let d = t["description"].as_str().unwrap();
            assert!(
                !d.is_empty() && !d.contains('\n'),
                "a one-line description: {d}"
            );
        }
        cleanup(&src);
    }

    #[test]
    fn bad_messages_get_json_rpc_errors() {
        let src = fixture("errors", None);
        let s = server(&src);
        let code = |line: &str| -> Value {
            let a: Value = serde_json::from_str(&s.handle_line(line).unwrap()).unwrap();
            a["error"]["code"].clone()
        };
        assert_eq!(code("{not json"), PARSE_ERROR);
        assert_eq!(code("[]"), INVALID_REQUEST, "an empty batch");
        assert_eq!(code("1"), INVALID_REQUEST);
        assert_eq!(code(r#"{"jsonrpc":"2.0","id":1}"#), INVALID_REQUEST);
        assert_eq!(
            code(r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#),
            INVALID_REQUEST
        );
        assert_eq!(
            code(r#"{"jsonrpc":"2.0","id":1,"method":"resources/list"}"#),
            METHOD_NOT_FOUND
        );
        assert_eq!(
            code(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#),
            INVALID_PARAMS
        );
        assert_eq!(
            code(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":[]}"#),
            INVALID_PARAMS
        );
        assert_eq!(
            code(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{}}"#),
            INVALID_PARAMS
        );
        assert_eq!(
            code(
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"repos","arguments":"x"}}"#
            ),
            INVALID_PARAMS
        );
        assert_eq!(
            code(
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"write_file"}}"#
            ),
            INVALID_PARAMS,
            "an unknown tool"
        );
        assert_eq!(
            s.handle_line(r#"{"jsonrpc":"2.0","id":5,"result":{}}"#),
            None,
            "a response"
        );
        // Bad arguments give tool results with `isError: true`.
        assert!(call_error(&s, "repos", json!({"window": "2h"})).contains("Unknown window"));
        assert!(call_error(&s, "repos", json!({"sort": "name"})).contains("Unknown argument"));
        assert!(call_error(&s, "repo", json!({})).contains("repo is required"));
        assert!(call_error(&s, "repo", json!({"repo": "gamma"}))
            .contains("example-org/alpha, example-org/beta"));
        assert!(
            call_error(&s, "pull_request", json!({"repo": "alpha", "number": "1"}))
                .contains("number")
        );
        assert!(
            call_error(&s, "pull_request", json!({"repo": "alpha", "number": 99}))
                .contains("No record")
        );
        assert!(call_error(&s, "my_prs", json!({"needs_me_only": "yes"})).contains("true or false"));
        cleanup(&src);
    }

    #[test]
    fn serve_answers_each_request_line_and_skips_notifications() {
        let src = fixture("serve", None);
        let input = [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "",
            r#"{"jsonrpc":"2.0","id":"two","method":"tools/list"}"#,
        ]
        .join("\n");
        let mut out = Vec::new();
        server(&src).serve(input.as_bytes(), &mut out).unwrap();
        let lines: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            (lines[0]["id"].clone(), lines[1]["id"].clone()),
            (json!(1), json!("two"))
        );
        cleanup(&src);
    }

    #[test]
    fn a_batch_gets_one_array_of_answers() {
        let src = fixture("batch", None);
        let s = server(&src);
        let batch = json!([
            {"jsonrpc": "2.0", "id": 1, "method": "ping"},
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
            {"jsonrpc": "2.0", "id": 3, "result": {}},
            7,
        ]);
        let a: Value = serde_json::from_str(&s.handle_line(&batch.to_string()).unwrap()).unwrap();
        let answers = a.as_array().expect("an array of answers");
        assert_eq!(answers.len(), 3, "{a}");
        assert_eq!(answers[0]["id"], 1);
        assert_eq!(answers[0]["result"], json!({}));
        assert_eq!(answers[1]["id"], 2);
        assert!(answers[1]["result"]["tools"].is_array());
        assert_eq!(answers[2]["error"]["code"], INVALID_REQUEST);
        let quiet = json!([
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            {"jsonrpc": "2.0", "id": 4, "result": {}},
        ]);
        assert_eq!(
            s.handle_line(&quiet.to_string()),
            None,
            "notifications and responses alone get no answer"
        );
        cleanup(&src);
    }

    #[test]
    fn repos_lists_each_row_with_its_data_age_most_urgent_first() {
        let src = fixture("repos", None);
        let s = server(&src);
        let a = call(&s, "repos", json!({}));
        assert_eq!(
            (a["window"].clone(), a["snapshot_found"].clone()),
            (json!("4h"), json!(true))
        );
        assert_eq!(a["rate_limit_remaining"], 4200);
        let rows = a["repos"].as_array().unwrap();
        let names: Vec<&str> = rows.iter().map(|r| r["repo"].as_str().unwrap()).collect();
        assert_eq!(names, ["example-org/beta", "example-org/alpha"]);
        let beta = &rows[0];
        assert_eq!(
            (beta["age_secs"].clone(), beta["stale"].clone()),
            (json!(3 * 3600), json!(true))
        );
        assert_eq!(beta["error"], "timed out");
        let alpha = &rows[1];
        assert_eq!(
            (alpha["age_secs"].clone(), alpha["stale"].clone()),
            (json!(3600), json!(false))
        );
        assert_eq!(alpha["recent"]["commits"], 5);
        assert_eq!(alpha["open_prs"], 3);
        assert!(alpha.get("icon").is_none(), "no icon data");
        assert_eq!(
            call(&s, "repos", json!({"window": "1d"}))["repos"][0]["recent"],
            Value::Null
        );
        cleanup(&src);
    }

    #[test]
    fn repo_gives_one_repos_stats_and_its_pull_requests() {
        let src = fixture("repo", None);
        let s = server(&src);
        let a = call(&s, "repo", json!({"repo": "ALPHA"}));
        let r = &a["repo"];
        assert_eq!(r["repo"], "example-org/alpha");
        assert_eq!(r["latest_build"], "v1.2.0");
        assert_eq!(r["recent_by_window"]["4h"]["commits"], 5);
        let p = &r["pull_requests"];
        assert_eq!(p["open"], 3);
        assert_eq!(p["main_checks"]["rollup"], "SUCCESS");
        assert_eq!(p["records_checked_at"], json!(t(18, 10)));
        assert_eq!(p["by_state"]["CI failing"], 1);
        assert_eq!(p["by_state"]["Not read yet"], 1);
        assert_eq!(p["flow_by_window"]["7d"]["merged"], 1);
        assert!(p["flow_by_window"]["1d"].is_null(), "merged 36 hours ago");
        let same = call(
            &s,
            "repo",
            json!({"repo": "https://github.com/Example-Org/Alpha"}),
        );
        assert_eq!(same["repo"]["repo"], "example-org/alpha");
        cleanup(&src);
    }

    #[test]
    fn repo_reads_a_respelled_repo_and_main_checks_without_records() {
        let src = fixture("respelled", None);
        let config = src.config.clone().unwrap();
        std::fs::write(
            &config,
            "repos = [\"Example-Org/Alpha\", \"example-org/beta\"]\n",
        )
        .unwrap();
        std::fs::remove_file(PrCache::path(&src.dir, "example-org/alpha")).unwrap();
        PrCache {
            main_checks: Some(MainChecks {
                oid: "beta-head".into(),
                rollup: Some("FAILURE".into()),
                failing: vec!["lint".into()],
            }),
            checked_at: Some(t(18, 8)),
            ..Default::default()
        }
        .save(&src.dir, "example-org/beta")
        .unwrap();
        let s = server(&src);
        let a = call(&s, "repo", json!({"repo": "alpha"}));
        assert_eq!(
            a["repo"]["open_prs"], 3,
            "the snapshot keeps the old spelling"
        );
        let p = &call(&s, "repo", json!({"repo": "beta"}))["repo"]["pull_requests"];
        assert_eq!(p["main_checks"]["failing"], json!(["lint"]));
        assert_eq!(p["records_checked_at"], json!(t(18, 8)));
        assert_eq!(p["open"], 0);
        cleanup(&src);
    }

    #[test]
    fn my_prs_lists_the_logins_pull_requests_with_blockers_and_checks() {
        let src = fixture("mine", Some("Ann-Example"));
        let s = server(&src);
        let a = call(&s, "my_prs", json!({}));
        assert_eq!(a["login"], "Ann-Example");
        assert_eq!(a["login_from"], "people.my_login in the config");
        let rows = a["rows"].as_array().unwrap();
        let got: Vec<(u64, &str, &str)> = rows
            .iter()
            .map(|r| {
                (
                    r["number"].as_u64().unwrap(),
                    r["blocker"].as_str().unwrap(),
                    r["tone"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                (7, "Conflicting", "me"),
                (1, "Failing checks", "me"),
                (2, "Not read yet", "unread")
            ]
        );
        let failing = &rows[1];
        assert_eq!(failing["failing_checks"], json!(["build (macos)"]));
        assert_eq!(failing["mergeable"], "MERGEABLE");
        assert_eq!(failing["merge_state"], "BLOCKED");
        assert_eq!(failing["checks_rollup"], "FAILURE");
        assert_eq!(failing["waits_on"], "you");
        assert_eq!(failing["needs_me"], true);
        assert_eq!(failing["held"], false);
        assert_eq!(failing["reply_needed"], Value::Null);
        assert!(failing["rule"].as_str().unwrap().starts_with("52:"));
        assert_eq!(rows[0]["mergeable"], "CONFLICTING");
        assert_eq!(
            rows[0]["in_state_secs"],
            60 * 3600,
            "conflicting since the 16th at 00:00"
        );
        assert_eq!(rows[2]["read"], false);
        assert_eq!(a["needs_me"], 2);
        let needs = call(&s, "my_prs", json!({"needs_me_only": true}));
        assert_eq!(needs["count"], 2);
        cleanup(&src);
    }

    #[test]
    fn my_prs_takes_the_kept_login_and_says_when_none_is_known() {
        let src = fixture("mine-viewer", None);
        let s = server(&src);
        let a = call(&s, "my_prs", json!({}));
        assert_eq!(a["login"], Value::Null);
        assert!(a["note"].as_str().unwrap().contains("people.my_login"));
        viewer::keep(&src.dir, "bo-example", t(18, 1)).unwrap();
        let a = call(&s, "my_prs", json!({}));
        assert_eq!(a["login"], "bo-example");
        assert_eq!(a["login_read_at"], json!(t(18, 1)));
        assert_eq!(a["rows"][0]["number"], 3);
        assert_eq!(a["count"], 1);
        cleanup(&src);
    }

    #[test]
    fn people_gives_the_work_rows_for_a_window_and_repo() {
        let src = fixture("people", None);
        let s = server(&src);
        let a = call(&s, "people", json!({}));
        assert_eq!(a["window"], "7d");
        let rows = a["rows"].as_array().unwrap();
        let ann = rows.iter().find(|r| r["key"] == "ann-example").unwrap();
        assert_eq!(
            (ann["open"].clone(), ann["merged"].clone()),
            (json!(3), json!(1))
        );
        assert_eq!(ann["name"], Value::Null, "no roster, no names");
        assert_eq!(a["records"], json!({"held": 5, "read": 4}));
        assert_eq!(
            a["records_checked_at"],
            json!({"example-org/alpha": t(18, 10), "example-org/beta": null})
        );
        let beta = call(&s, "people", json!({"repo": "beta"}));
        assert_eq!(beta["repo"], "example-org/beta");
        let ann = beta["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["key"] == "ann-example")
            .unwrap();
        assert_eq!(
            (ann["open"].clone(), ann["merged"].clone()),
            (json!(1), json!(0))
        );
        cleanup(&src);
    }

    #[test]
    fn pull_request_gives_one_record_with_its_work_state_and_rule() {
        let src = fixture("pr", Some("ann-example"));
        let s = server(&src);
        let a = call(
            &s,
            "pull_request",
            json!({"repo": "example-org/alpha", "number": 1}),
        );
        assert_eq!(a["github_state"], "open");
        assert_eq!(a["work"]["state"], "ci_failing_from_pr");
        assert!(a["work"]["rule"]
            .as_str()
            .unwrap()
            .contains("build (macos)"));
        assert_eq!(a["state_label"], "CI failing");
        assert_eq!(a["blocker"]["label"], "Failing checks");
        assert_eq!(a["checks"]["failing"], json!(["build (macos)"]));
        let merged = call(&s, "pull_request", json!({"repo": "alpha", "number": 4}));
        assert_eq!(
            (
                merged["github_state"].clone(),
                merged["work"]["state"].clone()
            ),
            (json!("merged"), json!("merged"))
        );
        assert!(merged["work"]["rule"].is_string());
        assert_eq!(merged["blocker"], Value::Null);
        cleanup(&src);
    }

    #[test]
    fn failing_checks_are_listed_only_while_the_rollup_has_failed() {
        let src = fixture("recovered", Some("bo-example"));
        // The checks failed, ran again and passed: the sweep read the new
        // rollup but not the names.
        let mut alpha = PrCache::load(&src.dir, "example-org/alpha");
        let rec = alpha.records.get_mut(&3).unwrap();
        rec.checks.rollup = Some("SUCCESS".into());
        rec.checks.failing = vec!["build (macos)".into()];
        alpha.save(&src.dir, "example-org/alpha").unwrap();
        let s = server(&src);
        let a = call(&s, "pull_request", json!({"repo": "alpha", "number": 3}));
        assert_eq!(a["checks"]["rollup"], "SUCCESS");
        assert_eq!(a["checks"]["failing"], json!([]));
        let mine = call(&s, "my_prs", json!({}));
        assert_eq!(mine["rows"][0]["number"], 3);
        assert_eq!(mine["rows"][0]["failing_checks"], json!([]));
        cleanup(&src);
    }

    /// Every file under `dir` with its length, time and contents.
    fn tree(dir: &Path) -> BTreeMap<PathBuf, (u64, std::time::SystemTime, Vec<u8>)> {
        let mut out = BTreeMap::new();
        let mut todo = vec![dir.to_path_buf()];
        while let Some(d) = todo.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                let m = std::fs::metadata(&p).unwrap();
                if m.is_dir() {
                    todo.push(p.clone());
                }
                let bytes = if m.is_file() {
                    std::fs::read(&p).unwrap()
                } else {
                    Vec::new()
                };
                out.insert(p, (m.len(), m.modified().unwrap(), bytes));
            }
        }
        out
    }

    #[test]
    fn no_tool_writes_a_file_or_shows_the_token() {
        let src = fixture("readonly", Some("ann-example"));
        viewer::keep(&src.dir, "ann-example", t(18, 1)).unwrap();
        let root = src.dir.parent().unwrap().to_path_buf();
        let before = tree(&root);
        let s = server(&src);
        let mut answers = String::new();
        for (tool, args) in [
            ("repos", json!({})),
            ("repo", json!({"repo": "alpha"})),
            ("my_prs", json!({})),
            ("people", json!({"window": "all"})),
            ("pull_request", json!({"repo": "beta", "number": 7})),
        ] {
            answers.push_str(&call(&s, tool, args).to_string());
        }
        assert_eq!(
            tree(&root),
            before,
            "the cache and the config are unchanged"
        );
        assert!(!answers.contains("not-a-real-token"));
        // A missing cache directory reads as no data and is not created.
        let missing = Sources {
            dir: root.join("nowhere"),
            config: src.config.clone(),
        };
        let a = call(&server(&missing), "repos", json!({}));
        assert_eq!(a["snapshot_found"], false);
        assert_eq!(a["repos"].as_array().unwrap().len(), 2);
        assert!(call(&server(&missing), "people", json!({}))["note"].is_string());
        assert!(!missing.dir.exists());
        cleanup(&src);
    }

    #[test]
    fn a_broken_config_gives_its_line_without_the_text() {
        let src = fixture("broken", None);
        let config = src.config.clone().unwrap();
        std::fs::write(
            &config,
            "repos = [\"example-org/alpha\"]\ngithub_token = \"not-a-real-token\n",
        )
        .unwrap();
        let message = call_error(&server(&src), "repos", json!({}));
        assert!(
            message.ends_with("is not valid TOML at line 2"),
            "{message}"
        );
        assert!(!message.contains("not-a-real-token"), "{message}");
        cleanup(&src);
    }
}
