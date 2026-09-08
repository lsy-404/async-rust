use async_rust_lib::{parse_sse_chunks, parse_sse_payload, AppState};

#[test]
fn sse_parser_ignores_done_and_extracts_deltas() {
    let stream = "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\ndata: [DONE]\n\ndata: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n";
    assert_eq!(parse_sse_payload(stream), vec!["hello", " world"]);
}

#[test]
fn database_initializes_without_sample_workspaces() {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open(directory.path().join("async.sqlite3")).unwrap();
    let db = rusqlite::Connection::open(state.database_path()).unwrap();
    let workspace_count: i64 = db
        .query_row("SELECT COUNT(*) FROM workspaces", [], |row| row.get(0))
        .unwrap();
    let provider: String = db
        .query_row("SELECT name FROM providers WHERE id='openai'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(workspace_count, 0);
    assert_eq!(provider, "OpenAI");
}

#[test]
fn sse_utf8_and_json_can_cross_network_chunks() {
    let line = "data: {\"choices\":[{\"delta\":{\"content\":\"你好\"}}]}\n".as_bytes();
    let split = line.iter().position(|byte| *byte >= 0x80).unwrap() + 1;
    let chunks = vec![line[..split].to_vec(), line[split..].to_vec()];
    assert_eq!(parse_sse_chunks(chunks).unwrap(), vec!["你好"]);
}
