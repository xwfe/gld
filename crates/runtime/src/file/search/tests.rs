use super::*;

/// The schema tells the client "default 100"; the code used to return up
/// to 1000, ten times what the client planned its context for.
#[test]
fn search_text_default_max_results_is_what_the_schema_says() {
    let dir = tempfile::tempdir().expect("dir");
    std::fs::write(dir.path().join("many.txt"), "needle\n".repeat(150)).expect("write");
    let ws = Workspace::new(dir.path().to_path_buf(), Vec::new()).expect("workspace");
    let result = search_text(&ws, &json!({ "query": "needle" })).expect("search");
    assert_eq!(
        result["matches"].as_array().expect("matches").len(),
        100,
        "{result}"
    );
    assert_eq!(result["truncated"], true);
}

/// Out-of-range values are brought into the declared range rather than
/// obeyed: a max_results of a billion is a request for "all of it", not
/// a licence to fill memory.
#[test]
fn search_text_max_results_above_the_schema_maximum_is_clamped() {
    let dir = tempfile::tempdir().expect("dir");
    std::fs::write(dir.path().join("many.txt"), "needle\n".repeat(10_050)).expect("write");
    let ws = Workspace::new(dir.path().to_path_buf(), Vec::new()).expect("workspace");
    let result = search_text(
        &ws,
        &json!({ "query": "needle", "max_results": 1_000_000_000u64, "max_preview_bytes": 1 }),
    )
    .expect("search");
    assert_eq!(result["matches"].as_array().expect("matches").len(), 10_000);
}

/// 搜索换了读行的实现（`BufRead::lines()` → 共用的 `toexec_text::next_line`）。
/// `lines()` 会把 `\r\n` 两个字节都去掉，新实现必须一样，否则 CRLF 文件里
/// 每条 preview 末尾都会多一个 `\r`，行尾锚定的正则也会失配。
#[test]
fn a_crlf_file_is_searched_without_the_carriage_return() {
    let dir = tempfile::tempdir().expect("dir");
    std::fs::write(dir.path().join("crlf.txt"), "alpha\r\nneedle\r\nomega\r\n").expect("write");
    let ws = Workspace::new(dir.path().to_path_buf(), Vec::new()).expect("workspace");

    let result = search_text(&ws, &json!({ "query": "needle" })).expect("search");
    let matches = result["matches"].as_array().expect("matches");
    assert_eq!(matches.len(), 1, "{result}");
    assert_eq!(matches[0]["preview"], "needle");
    assert_eq!(matches[0]["line"], 2);

    // 行尾锚定：`\r` 要是还留着，这条就搜不到。
    let anchored = search_text(
        &ws,
        &json!({ "query": "needle$", "regex": true, "context_lines": 1 }),
    )
    .expect("search");
    assert_eq!(anchored["matches"].as_array().expect("matches").len(), 1);
    assert_eq!(anchored["matches"][0]["before"][0], "alpha");
    assert_eq!(anchored["matches"][0]["after"][0], "omega");
}

/// 新语义，跟以前不一样：一行超过 `SEARCH_LINE_KEEP` 的部分不再参与匹配。
/// 换来的是一行占的内存有上限——以前有多长就吃多长，`context_lines` 还会
/// 把它克隆几十份。
#[test]
fn a_line_past_the_keep_limit_is_only_searched_up_to_it() {
    let dir = tempfile::tempdir().expect("dir");
    let padding = "x".repeat(SEARCH_LINE_KEEP);
    std::fs::write(
        dir.path().join("huge.txt"),
        format!("needle-early{padding}needle-late\n"),
    )
    .expect("write");
    let ws = Workspace::new(dir.path().to_path_buf(), Vec::new()).expect("workspace");
    let big = json!({ "max_file_bytes": 67_108_864u64 });

    let early = search_text(
        &ws,
        &json!({ "query": "needle-early", "max_file_bytes": big["max_file_bytes"] }),
    )
    .expect("search");
    assert_eq!(early["matches"].as_array().expect("matches").len(), 1);

    let late = search_text(
        &ws,
        &json!({ "query": "needle-late", "max_file_bytes": big["max_file_bytes"] }),
    )
    .expect("search");
    assert!(
        late["matches"].as_array().expect("matches").is_empty(),
        "保留上限之后的内容不该被搜到：{late}"
    );
    // 搜不到和"这一行根本没搜完"是两件事，结果里必须分得出来（方案 D）。
    assert_eq!(late["lines_truncated_for_search"], 1, "{late}");
    let warnings = late["warnings"].as_array().expect("warnings");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap_or_default().contains("longer than")),
        "没搜完的那一行要说出来：{late}"
    );
    // 正常长度的文件不该背上这条提示。
    std::fs::write(dir.path().join("small.txt"), "needle-early\n").expect("write");
    let small = search_text(
        &ws,
        &json!({ "query": "needle-early", "path": "small.txt" }),
    )
    .expect("search");
    assert_eq!(small["lines_truncated_for_search"], 0, "{small}");
}

/// 结果里所有字符串加起来多少字节。context 的克隆全都落在这里，所以这个
/// 数就是放大程度，不用去量进程 RSS。
fn held_string_bytes(value: &Value) -> usize {
    match value {
        Value::String(text) => text.len(),
        Value::Array(items) => items.iter().map(held_string_bytes).sum(),
        Value::Object(fields) => fields.values().map(held_string_bytes).sum(),
        _ => 0,
    }
}

/// context 行不截断的话，`context_lines: 20` 会把整行克隆几十份（为什么克隆
/// 这么多份，见 `search_file_streaming` 里的注释）。
///
/// 下面这份 41 行、每行 64 KiB 的文件，修之前结果里的字符串有 79,972,193 字节、
/// 进程峰值 RSS 212 MB，修之后是 326,933 字节、RSS 11 MB；1 MiB 的阈值卡在中间。
#[test]
fn long_context_lines_are_clipped_instead_of_cloned_whole() {
    let dir = tempfile::tempdir().expect("dir");
    let wide = "x".repeat(64 * 1024);
    std::fs::write(
        dir.path().join("wide.txt"),
        format!("needle{wide}\n").repeat(41),
    )
    .expect("write");
    let ws = Workspace::new(dir.path().to_path_buf(), Vec::new()).expect("workspace");

    let result = search_text(
        &ws,
        &json!({
            "query": "needle",
            "context_lines": 20,
            "max_file_bytes": 67_108_864u64,
        }),
    )
    .expect("search");

    let matches = result["matches"].as_array().expect("matches");
    assert_eq!(matches.len(), 41);
    let held = held_string_bytes(&result);
    assert!(
        held < 1024 * 1024,
        "context 行还在整行克隆：结果里有 {held} 字节"
    );

    // 行为变了，钉死：超长的 context 行按 max_preview_bytes（默认 256）截断，
    // 跟 preview 一样以 ... 收尾。
    let before = matches[1]["before"][0].as_str().expect("before");
    assert_eq!(before, matches[1]["preview"].as_str().expect("preview"));
    assert_eq!(before.len(), 256 + "...".len());
    assert_eq!(
        matches[1]["after"][0].as_str().expect("after").len(),
        256 + "...".len()
    );
}
