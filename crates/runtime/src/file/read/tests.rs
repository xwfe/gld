use super::*;

/// 改成流式之前的算法，原样留着当标准答案。
fn whole_file_reference(
    data: &[u8],
    start_line: usize,
    end_line: Option<usize>,
    max_bytes: usize,
) -> Result<(String, bool, usize), &'static str> {
    if data.iter().take(4096).any(|b| *b == 0) {
        return Err("BINARY_FILE");
    }
    let text = std::str::from_utf8(data).map_err(|_| "UNSUPPORTED_ENCODING")?;
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let total_lines = lines.len();
    let end = end_line.unwrap_or(total_lines).min(total_lines);
    let selected: String = if end < start_line {
        String::new()
    } else {
        lines[(start_line - 1)..end].concat()
    };
    if selected.len() <= max_bytes {
        return Ok((selected, false, total_lines));
    }
    let mut cut = max_bytes;
    while cut > 0 && !selected.is_char_boundary(cut) {
        cut -= 1;
    }
    Ok((selected[..cut].to_string(), true, total_lines))
}

fn error_code(error: WorkspaceError) -> &'static str {
    match error {
        WorkspaceError::Tool { code, .. } | WorkspaceError::ToolDetails { code, .. } => code,
    }
}

/// 分块大小故意压到 1、2、3 字节，让多字节字符、换行符、嗅探窗口的边界都落在两块之间。
fn assert_same_as_reference(data: &[u8]) {
    let ranges: &[(usize, Option<usize>)] = &[
        (1, None),
        (1, Some(1)),
        (2, Some(3)),
        (3, None),
        (5, Some(3)),
        (10_000, None),
    ];
    for chunk in [1, 2, 3, 5, 4096, READ_CHUNK_BYTES] {
        for &(start, end) in ranges {
            for max_bytes in [0, 1, 4, 7, 32, usize::MAX / 2] {
                let expected = whole_file_reference(data, start, end, max_bytes);
                let actual = read_text_selection(data, chunk, start, end, max_bytes)
                    .map(|s| {
                        assert_eq!(s.total_bytes, data.len() as u64);
                        (s.content, s.truncated, s.total_lines)
                    })
                    .map_err(error_code);
                assert_eq!(
                    actual,
                    expected,
                    "chunk={chunk} start={start} end={end:?} max_bytes={max_bytes} data={:?}",
                    String::from_utf8_lossy(&data[..data.len().min(80)])
                );
            }
        }
    }
}

#[test]
fn streaming_read_matches_the_whole_file_algorithm() {
    for text in [
        "",
        "\n",
        "\n\n\n",
        "no newline at all",
        "a\nbb\nccc\n",
        "a\nbb\nccc",
        "crlf\r\nline\r\n",
        "中文\n混合 emoji 🌬 和 ascii\n最后一行没有换行",
        "🌬🌬🌬🌬🌬🌬🌬🌬🌬🌬🌬🌬🌬🌬🌬🌬🌬🌬🌬🌬",
    ] {
        assert_same_as_reference(text.as_bytes());
    }
}

#[test]
fn streaming_read_reports_the_same_errors_as_before() {
    let mut nul_inside_sniff = vec![b'a'; 4095];
    nul_inside_sniff.push(0);
    let mut nul_after_sniff = vec![b'a'; 4096];
    nul_after_sniff.push(0);
    // 非法 UTF-8 在前、0 字节在后（仍在嗅探窗口内）：以前报的是 BINARY_FILE。
    let mut invalid_then_nul = b"ok\n\xff".to_vec();
    invalid_then_nul.extend_from_slice(&[b'a'; 100]);
    invalid_then_nul.push(0);

    for data in [
        nul_inside_sniff,
        nul_after_sniff,
        invalid_then_nul,
        b"lone continuation \x80 byte\n".to_vec(),
        // 文件在多字节字符中间结束。
        "尾巴被截断".as_bytes()[..7].to_vec(),
    ] {
        assert_same_as_reference(&data);
    }
}

#[test]
fn a_giant_single_line_is_cut_without_holding_it_in_memory() {
    let data = "字".repeat(100_000);
    let selection = read_text_selection(data.as_bytes(), 1024, 1, None, 10).expect("read");
    assert_eq!(selection.content, "字字字");
    assert!(selection.truncated);
    assert_eq!(selection.total_lines, 1);
    assert_eq!(selection.total_bytes, data.len() as u64);
}
