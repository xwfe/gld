//! 收摊的两条路：`gld stop --all` 只是停下来，`gld destroy` 是真的删掉。
//!
//! 这两件事很容易被当成一件——都让服务不再跑，输出看着也差不多。但代价完全
//! 不同：stop 之后 `gld start` 立刻还能用，destroy 把密钥一起删了，
//! 客户端里存的 token / 口令全部失效，而且没有备份。
//!
//! 所以这里盯三件事：stop 不许碰配置、destroy 必须先确认、两者都不许动项目文件。

mod common;

use common::env::{free_port, Env};

/// 起两个工作区，`stop --all` 要把它们都停掉，而配置一个不少。
#[test]
fn stop_all_stops_every_workspace_but_deletes_nothing() {
    let env = Env::new();
    let other = tempfile::tempdir().expect("second project");
    env.ok(&[
        "start",
        env.project.path().to_str().unwrap(),
        "--port",
        &free_port().to_string(),
    ]);
    env.ok(&[
        "start",
        other.path().to_str().unwrap(),
        "--port",
        &free_port().to_string(),
    ]);
    assert_eq!(
        env.json(&["--json", "ps"]).as_array().map(Vec::len),
        Some(2),
        "两个工作区都该在跑"
    );

    env.ok(&["stop", "--all"]);

    assert_eq!(
        env.json(&["--json", "ps"]).as_array().map(Vec::len),
        Some(0),
        "stop --all 之后还有服务在跑"
    );
    assert_eq!(
        env.json(&["--json", "ws", "list"]).as_array().map(Vec::len),
        Some(2),
        "stop 不该删掉任何工作区"
    );
    // 停了还能起回来——这正是它和 destroy 的分界。
    env.ok(&["start"]);
    assert_eq!(
        env.json(&["--json", "ps"]).as_array().map(Vec::len),
        Some(1)
    );
}

/// `destroy <工作区>`：服务先停，配置和密钥一起消失，项目文件还在。
#[test]
fn destroy_removes_one_workspace_and_leaves_the_project_alone() {
    let env = Env::new();
    env.write("keep-me.txt", "still here\n");
    env.ok(&[
        "start",
        env.project.path().to_str().unwrap(),
        "--port",
        &free_port().to_string(),
    ]);
    let name = env.json(&["--json", "ws", "list"])[0]["name"]
        .as_str()
        .expect("name")
        .to_string();

    env.ok(&["destroy", &name, "-y"]);

    assert_eq!(
        env.json(&["--json", "ws", "list"]).as_array().map(Vec::len),
        Some(0)
    );
    assert_eq!(
        env.json(&["--json", "ps"]).as_array().map(Vec::len),
        Some(0),
        "销毁前要先把服务停掉"
    );
    assert!(
        env.project.path().join("keep-me.txt").is_file(),
        "项目文件被删了"
    );
}

/// `destroy --all` 清空全部。
#[test]
fn destroy_all_clears_every_workspace() {
    let env = Env::new();
    let other = tempfile::tempdir().expect("second project");
    env.ok(&["ws", "add", ".", "--name", "one"]);
    env.ok(&["ws", "add", other.path().to_str().unwrap(), "--name", "two"]);

    let text = env.ok(&["destroy", "--all", "-y"]);
    assert!(text.contains("one") && text.contains("two"), "{text}");
    assert_eq!(
        env.json(&["--json", "ws", "list"]).as_array().map(Vec::len),
        Some(0)
    );
}

/// 不给 `-y` 又没法交互时必须停下来，而且要先说清楚要删的是谁。
///
/// 密钥删了就没了。脚本里漏写 `-y` 的话，能问的时候问、问不了就退出，
/// 比"默默照删"强得多。
#[test]
fn destroy_refuses_to_guess_when_it_cannot_ask() {
    let env = Env::new();
    env.ok(&["ws", "add", ".", "--name", "precious"]);

    let output = env.gld(&["destroy"]);
    assert!(!output.status.success(), "没确认就删了");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("precious"),
        "要先列出将被销毁的工作区：{stdout}"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("-y"));
    assert_eq!(
        env.json(&["--json", "ws", "list"]).as_array().map(Vec::len),
        Some(1),
        "被拒绝的销毁不能留下半个状态"
    );
}

/// 位置参数和 `-w` 同时给出时意图矛盾——删除这种事不能靠猜。
#[test]
fn destroy_refuses_a_target_given_twice() {
    let env = Env::new();
    env.ok(&["ws", "add", ".", "--name", "one"]);

    let output = env.gld(&["destroy", "one", "-w", "one", "-y"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("不知道该听哪个"));
    assert_eq!(
        env.json(&["--json", "ws", "list"]).as_array().map(Vec::len),
        Some(1)
    );
}
