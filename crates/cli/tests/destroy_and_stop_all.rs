//! 收摊的两条路：`gld stop` 只是停下来，`gld rm` 是真的删掉。
//!
//! 这两件事很容易被当成一件——都让 AI 用不了这个项目，输出看着也差不多。但代价
//! 完全不同：stop 之后 `gld start` 立刻还能用；rm 把项目在 gld 这边的配置和记账
//! 一起删了，而且没有备份。
//!
//! 所以这里盯三件事：stop 不许碰配置、rm 必须先确认、两者都不许动项目文件。

mod common;

use common::env::{free_port, Env};

/// 服务里现在有几个项目。
fn projects(env: &Env) -> usize {
    env.json(&["--json", "ls"])["service"]["members"]
        .as_array()
        .map(Vec::len)
        .expect("ls 里要有项目表")
}

fn service_state(env: &Env) -> String {
    env.json(&["--json", "status"])["service"]["state"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// 两个项目挂在服务下，`stop` 把服务停掉，而项目一个不少。
#[test]
fn stop_stops_the_service_but_deletes_nothing() {
    let env = Env::new();
    let other = tempfile::tempdir().expect("second project");
    env.ok(&[
        "start",
        env.project.path().to_str().unwrap(),
        "--port",
        &free_port().to_string(),
    ]);
    // 第二次 start 带目录：把它加进同一个服务，不是再起一个。
    env.ok(&["start", other.path().to_str().unwrap()]);
    assert_eq!(projects(&env), 2, "两个项目都该在服务里");
    assert_eq!(service_state(&env), "running");

    env.ok(&["stop"]);

    assert_eq!(service_state(&env), "stopped", "stop 之后服务还在跑");
    assert_eq!(projects(&env), 2, "stop 不该删掉任何项目");
    // 停了还能起回来——这正是它和 rm 的分界。
    env.ok(&["start"]);
    assert_eq!(service_state(&env), "running");
}

/// `rm <项目>`：从服务里拿掉，配置一起消失，项目文件还在。
#[test]
fn rm_removes_one_project_and_leaves_its_files_alone() {
    let env = Env::new();
    env.write("keep-me.txt", "still here\n");
    env.ok(&[
        "start",
        env.project.path().to_str().unwrap(),
        "--port",
        &free_port().to_string(),
    ]);
    let name = env.json(&["--json", "ls"])["service"]["members"][0]["name"]
        .as_str()
        .expect("name")
        .to_string();

    env.ok(&["rm", &name, "-y"]);

    assert_eq!(projects(&env), 0);
    assert!(
        env.project.path().join("keep-me.txt").is_file(),
        "项目文件被删了"
    );
}

/// `rm --all` 清空全部；旧写法 `destroy` 还认。
#[test]
fn rm_all_clears_every_project_and_destroy_still_works() {
    let env = Env::new();
    let other = tempfile::tempdir().expect("second project");
    env.ok(&["add", ".", "--name", "one"]);
    env.ok(&["add", other.path().to_str().unwrap(), "--name", "two"]);

    let text = env.ok(&["destroy", "--all", "-y"]);
    assert!(text.contains("one") && text.contains("two"), "{text}");
    assert_eq!(projects(&env), 0);
}

/// 不给 `-y` 又没法交互时必须停下来，而且要先说清楚要删的是谁。
///
/// 配置删了就没了。脚本里漏写 `-y` 的话，能问的时候问、问不了就退出，
/// 比"默默照删"强得多。
#[test]
fn rm_refuses_to_guess_when_it_cannot_ask() {
    let env = Env::new();
    env.ok(&["add", ".", "--name", "precious"]);

    let output = env.gld(&["rm"]);
    assert!(!output.status.success(), "没确认就删了");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("precious"),
        "要先列出将被删除的项目：{stdout}"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("-y"));
    assert_eq!(projects(&env), 1, "被拒绝的删除不能留下半个状态");
}

/// 位置参数和 `-w` 同时给出时意图矛盾——删除这种事不能靠猜。
#[test]
fn rm_refuses_a_target_given_twice() {
    let env = Env::new();
    env.ok(&["add", ".", "--name", "one"]);

    let output = env.gld(&["rm", "one", "-w", "one", "-y"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("不知道该听哪个"));
    assert_eq!(projects(&env), 1);
}
