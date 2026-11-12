use super::*;

const SAMPLE: &str = r##"{
 "cells": [
  {
   "cell_type": "code",
   "execution_count": 1,
   "id": "aaaa1111",
   "metadata": {},
   "outputs": [
    {
     "name": "stdout",
     "output_type": "stream",
     "text": [
      "hello\n"
     ]
    }
   ],
   "source": [
    "print('hello')\n"
   ]
  },
  {
   "cell_type": "markdown",
   "id": "bbbb2222",
   "metadata": {},
   "source": [
    "# Title\n"
   ]
  }
 ],
 "metadata": {
  "language_info": {
   "name": "python"
  }
 },
 "nbformat": 4,
 "nbformat_minor": 5
}
"##;

/// 真实 Jupyter 写法的样本，和 ccnm 用的是**同一份**——那边拿
/// nbformat 5.11.1 逐字节核对过。同一份输入两个产品写回去应该一样，
/// 不一样就说明有一边偏了。
const REAL: &str = include_str!("../../tests/fixtures/notebook/analysis.ipynb");

fn parse() -> Notebook {
    Notebook::parse(SAMPLE.as_bytes(), "a.ipynb").expect("parse")
}

/// 没改过的 notebook 写回去必须**逐字节**一样。差一个空格，第一次改动
/// 就会在 git diff 里变成整份文件重写，没人看得出真正改了什么。
#[test]
fn an_untouched_notebook_round_trips_byte_for_byte() {
    let notebook = parse();
    assert_eq!(
        String::from_utf8(notebook.to_bytes()).expect("utf-8"),
        SAMPLE
    );
}

#[test]
fn rendering_shows_cells_and_their_outputs() {
    let notebook = parse();
    let rendered = render(&notebook, 0, 64 * 1024);
    assert_eq!(rendered.shown, Some((0, 1)));
    assert_eq!(rendered.next_start_cell, None);
    assert!(
        rendered
            .text
            .contains("<cell id=\"aaaa1111\" index=\"0\" type=\"code\" execution_count=\"1\">"),
        "{}",
        rendered.text
    );
    assert!(rendered.text.contains("print('hello')"));
    assert!(rendered
        .text
        .contains("<output cell=\"aaaa1111\" type=\"stream\" name=\"stdout\">"));
    assert!(rendered.text.contains("hello"));
    assert!(rendered.text.contains("<cell id=\"bbbb2222\""));
}

/// 一页装不下就停在 cell 边界上，并说下一页从哪儿开始。
#[test]
fn paging_stops_on_a_cell_boundary() {
    let notebook = parse();
    let rendered = render(&notebook, 0, 10);
    assert_eq!(rendered.shown, Some((0, 0)));
    assert_eq!(rendered.next_start_cell, Some(1));
    assert!(!rendered.text.contains("bbbb2222"), "{}", rendered.text);

    let second = render(&notebook, 1, 10);
    assert_eq!(second.shown, Some((1, 1)));
    assert_eq!(second.next_start_cell, None);
}

#[test]
fn replacing_a_code_cell_clears_its_outputs() {
    let edited = edit(
        SAMPLE.as_bytes(),
        "a.ipynb",
        &[CellEdit {
            cell_id: Some("aaaa1111".into()),
            new_source: Some("print('bye')\n".into()),
            cell_type: None,
            edit_mode: "replace".into(),
        }],
    )
    .expect("edit");
    let notebook = Notebook::parse(&edited, "a.ipynb").expect("parse");
    let cell = &notebook.cells()[0];
    assert_eq!(cell["source"], serde_json::json!(["print('bye')\n"]));
    assert_eq!(cell["outputs"], serde_json::json!([]));
    assert_eq!(cell["execution_count"], Value::Null);
}

/// 换成 markdown 的 cell 不能留着 outputs / execution_count：
/// nbformat 的 schema 不允许，留着的文件 Jupyter 打不开。
#[test]
fn turning_a_code_cell_into_markdown_drops_the_code_only_keys() {
    let edited = edit(
        SAMPLE.as_bytes(),
        "a.ipynb",
        &[CellEdit {
            cell_id: Some("aaaa1111".into()),
            new_source: Some("# now prose\n".into()),
            cell_type: Some("markdown".into()),
            edit_mode: "replace".into(),
        }],
    )
    .expect("edit");
    let notebook = Notebook::parse(&edited, "a.ipynb").expect("parse");
    let cell = notebook.cells()[0].as_object().expect("object");
    assert_eq!(cell["cell_type"], "markdown");
    assert!(!cell.contains_key("outputs"), "{cell:?}");
    assert!(!cell.contains_key("execution_count"), "{cell:?}");
}

#[test]
fn inserting_after_a_cell_gets_a_fresh_id() {
    let edited = edit(
        SAMPLE.as_bytes(),
        "a.ipynb",
        &[CellEdit {
            cell_id: Some("aaaa1111".into()),
            new_source: Some("x = 1\n".into()),
            cell_type: Some("code".into()),
            edit_mode: "insert".into(),
        }],
    )
    .expect("edit");
    let notebook = Notebook::parse(&edited, "a.ipynb").expect("parse");
    assert_eq!(notebook.cells().len(), 3);
    let inserted = notebook.cells()[1].as_object().expect("object");
    assert_eq!(inserted["source"], serde_json::json!(["x = 1\n"]));
    let id = inserted["id"].as_str().expect("id");
    assert_eq!(id.len(), 8);
    assert_ne!(id, "aaaa1111");
}

/// 没有 id 的老 notebook 用 `cell-N` 定位，和 Claude Code 一样。
#[test]
fn a_notebook_without_ids_is_addressed_by_index() {
    let old = r#"{"cells":[{"cell_type":"code","metadata":{},"outputs":[],"source":["a\n"]}],"metadata":{},"nbformat":4,"nbformat_minor":2}"#;
    let edited = edit(
        old.as_bytes(),
        "old.ipynb",
        &[CellEdit {
            cell_id: Some("cell-0".into()),
            new_source: Some("b\n".into()),
            cell_type: None,
            edit_mode: "replace".into(),
        }],
    )
    .expect("edit");
    let notebook = Notebook::parse(&edited, "old.ipynb").expect("parse");
    assert_eq!(notebook.cells()[0]["source"], serde_json::json!(["b\n"]));
    // 4.2 的 notebook 里插进去的 cell 不该凭空多出一个 id。
    let inserted = edit(
        old.as_bytes(),
        "old.ipynb",
        &[CellEdit {
            cell_id: None,
            new_source: Some("c\n".into()),
            cell_type: Some("code".into()),
            edit_mode: "insert".into(),
        }],
    )
    .expect("insert");
    let notebook = Notebook::parse(&inserted, "old.ipynb").expect("parse");
    assert!(notebook.cells()[0].get("id").is_none());
}

#[test]
fn an_unknown_cell_id_lists_the_ones_that_exist() {
    let error = edit(
        SAMPLE.as_bytes(),
        "a.ipynb",
        &[CellEdit {
            cell_id: Some("nope".into()),
            new_source: Some("x\n".into()),
            cell_type: None,
            edit_mode: "replace".into(),
        }],
    )
    .expect_err("no such cell");
    let message = error.message();
    assert!(
        message.contains("aaaa1111") && message.contains("bbbb2222"),
        "{message}"
    );
}

#[test]
fn a_file_that_is_not_a_notebook_says_so() {
    let error = Notebook::parse(b"{\"a\": 1}", "x.ipynb").expect_err("not a notebook");
    assert!(error.message().contains("not a Jupyter notebook"));
    let error = Notebook::parse(b"not json", "x.ipynb").expect_err("not json");
    assert!(error.message().contains("not valid JSON"));
}

/// nbformat 3 是另一套结构，别假装能改。
#[test]
fn nbformat_three_is_refused_with_a_way_out() {
    let old = r#"{"cells":[],"nbformat":3,"nbformat_minor":0}"#;
    let error = Notebook::parse(old.as_bytes(), "old.ipynb").expect_err("nbformat 3");
    assert!(error.message().contains("nbconvert"), "{}", error.message());
}

/// nbformat 写的那份，读进来再写回去必须一个字节都不差：中文不转义、
/// 键排序、缩进 1 格、末尾换行。差一点，第一次改动就会在 git diff 里
/// 变成整份文件重写。
#[test]
fn a_real_jupyter_notebook_round_trips_byte_for_byte() {
    let notebook = Notebook::parse(REAL.as_bytes(), "analysis.ipynb").expect("parse");
    assert_eq!(String::from_utf8(notebook.to_bytes()).expect("utf-8"), REAL);
    assert_eq!(notebook.cells().len(), 5);
    assert_eq!(notebook.format(), "4.5");
}

/// 四种输出各渲染成什么：stream 给文本，display_data 的图片只报大小，
/// execute_result 给 text/plain，error 去掉颜色码。
#[test]
fn every_kind_of_output_is_rendered() {
    let notebook = Notebook::parse(REAL.as_bytes(), "analysis.ipynb").expect("parse");
    let text = render(&notebook, 0, 1024 * 1024).text;
    assert!(text.contains("# 销售分析"), "中文原样给出：{text}");
    assert!(text.contains("<output cell=\"b7d3a901\" type=\"stream\" name=\"stdout\">"));
    assert!(text.contains("rows: 3"));
    assert!(
        text.contains("[image/png, about") && text.contains("not included"),
        "图片要说有多大、并说明没发出来：{text}"
    );
    assert!(text.contains("<output cell=\"d0f19b3c\" type=\"error\">"));
    assert!(
        text.contains("ZeroDivisionError: division by zero"),
        "{text}"
    );
    assert!(!text.contains('\u{1b}'), "traceback 的颜色码没去掉：{text}");
}

#[test]
fn a_traceback_loses_its_colour_codes() {
    assert_eq!(
        strip_ansi("\u{1b}[0;31mValueError\u{1b}[0m: bad"),
        "ValueError: bad"
    );
}
