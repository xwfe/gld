//! `gld start <目录>` / `gld list` / `gld upgrade`：一条命令从零到能用。
//!
//! 这三条命令合起来替掉了以前的四步（`workspace add` → `start` → `connect`
//! → `ws set` + `restart`）。合并之后新出现的风险都在"到底作用在哪个工作区"上：
//!
//! 1. 自动登记不能张冠李戴——在一个全新目录里敲 `gld start`，如果系统里
//!    正好只有一个工作区，旧的推断规则会把那个不相干的工作区拿来启动；
//! 2. 已登记项目的子目录里不能又建一个——那样同一个仓库会有两个工作区，
//!    两套端口两套密钥，而列表里看着像两个项目；
//! 3. `-w` 拼错时不能顺手建一个：那是打字错误，不是新项目。

mod common;

use common::env::{free_port, Env};

/// 目录没登记过，一条 `gld start <目录>` 就该登记 + 启动。
#[test]
fn start_registers_and_launches_a_directory_that_was_never_added() {
    let env = Env::new();
    let port = free_port();
    let text = env.ok(&[
        "start",
        env.project.path().to_str().unwrap(),
        "--port",
        &port.to_string(),
    ]);

    assert!(text.contains("已登记工作区"), "没有告知自动登记：{text}");
    assert_eq!(
        env.json(&["--json", "ws", "list"]).as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(
        env.json(&["--json", "ps"]).as_array().map(Vec::len),
        Some(1),
        "服务没起来"
    );
}

/// 在一个全新目录里敲 `gld start`，不能把碰巧唯一的那个工作区拿来启动。
///
/// 旧的工作区推断有一条"只有一个工作区就用它"的回退，对只读命令很方便，
/// 但套到 start 上就是：你在 ~/code/new 里启动，跑起来的却是 ~/code/old，
/// 而输出里除了端口没有任何地方能看出这一点。
#[test]
fn start_in_a_new_directory_never_hijacks_the_only_existing_workspace() {
    let env = Env::new();
    env.ok(&[
        "ws",
        "add",
        ".",
        "--name",
        "first",
        "--mcp-port",
        &free_port().to_string(),
    ]);

    let other = tempfile::tempdir().expect("another project");
    let text = env.ok(&[
        "start",
        other.path().to_str().unwrap(),
        "--port",
        &free_port().to_string(),
    ]);
    assert!(text.contains("已登记工作区"), "{text}");

    let names: Vec<String> = env
        .json(&["--json", "ws", "list"])
        .as_array()
        .expect("list")
        .iter()
        .map(|item| item["name"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        names.len(),
        2,
        "第二个目录没被登记成自己的工作区：{names:?}"
    );
    assert!(names.iter().any(|name| name == "first"));
}

/// 已登记项目的子目录属于那个项目，不该再建一个。
#[test]
fn start_inside_a_subdirectory_reuses_the_workspace_that_owns_it() {
    let env = Env::new();
    let port = free_port();
    env.ok(&[
        "start",
        env.project.path().to_str().unwrap(),
        "--port",
        &port.to_string(),
    ]);
    let sub = env.project.path().join("src/deep");
    std::fs::create_dir_all(&sub).expect("subdir");

    let text = env.ok(&["start", sub.to_str().unwrap()]);
    assert!(
        !text.contains("已登记工作区"),
        "子目录被当成新项目登记了：{text}"
    );
    assert_eq!(
        env.json(&["--json", "ws", "list"]).as_array().map(Vec::len),
        Some(1)
    );
}

/// 相对路径要按用户敲命令时所在的目录算，不能按守护进程的工作目录算。
///
/// 守护进程的工作目录是数据目录。以前 CLI 把 `../ccnm` 原样转发过去，守护进程
/// 就按数据目录去解析——`~/.config` 底下正好有个同名目录的话（很多工具都往那儿
/// 放配置），gld 会一声不吭地把别人的配置目录登记成工作区。用户看到的现象是
/// 「同一条命令跑两次，冒出两个工作区」，而第二个指着一个毫不相干的目录。
#[test]
fn a_relative_path_is_resolved_where_the_user_typed_it() {
    let env = Env::new();
    // 守护进程必须先跑起来：它没跑的时候请求在 CLI 进程里就地执行，
    // 那时的工作目录本来就是对的，复现不出问题。
    env.ok(&["daemon", "start"]);

    let real = env.project.path().join("ccnm");
    std::fs::create_dir_all(&real).expect("project dir");
    // 陷阱：数据目录旁边放一个同名目录，解析基准错了就会命中它。
    std::fs::create_dir_all(env.home.path().join("ccnm")).expect("decoy dir");

    env.ok(&[
        "ws",
        "add",
        "ccnm",
        "--name",
        "proj",
        "--mcp-port",
        &free_port().to_string(),
    ]);

    let list = env.json(&["--json", "ws", "list"]);
    let registered = list[0]["path"].as_str().expect("path").to_string();
    assert_eq!(
        std::path::Path::new(&registered),
        real.canonicalize().expect("canonical project dir"),
        "登记到了别的目录去"
    );

    // selector 也一样：`gld destroy ccnm` 里的 ccnm 是相对路径，
    // 名字（proj）对不上，只能靠路径匹配。
    env.ok(&["destroy", "ccnm", "-y"]);
    assert_eq!(
        env.json(&["--json", "ws", "list"]).as_array().map(Vec::len),
        Some(0),
        "相对路径 selector 没匹配上"
    );
}

/// `-w` 写了个不存在的名字 = 打字错误，要报错并列候选，而不是建一个新的。
#[test]
fn a_typo_in_dash_w_is_an_error_not_a_new_workspace() {
    let env = Env::new();
    env.ok(&["ws", "add", ".", "--name", "real"]);

    let output = env.gld(&["start", "-w", "rael"]);
    assert!(!output.status.success(), "拼错的工作区名被接受了");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("未找到工作区"), "{stderr}");
    assert_eq!(
        env.json(&["--json", "ws", "list"]).as_array().map(Vec::len),
        Some(1),
        "拼错名字不该多出一个工作区"
    );
}

/// 目录和 `-w` 同时给出时意图矛盾，要当场问清楚而不是挑一个。
#[test]
fn a_path_and_dash_w_together_are_refused() {
    let env = Env::new();
    env.ok(&["ws", "add", ".", "--name", "one"]);

    let output = env.gld(&["start", env.project.path().to_str().unwrap(), "-w", "one"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("不知道该听哪个"));
}

/// 用路径挑了工作区却没说要改什么时，得先讲清那个路径是干什么用的。
///
/// `-w <路径>` 和 `--path <路径>` 都收路径，方向却相反：一个是"改哪个工作区"，
/// 一个是"把目录改成这个"。光回一句"没说要改什么。可改的：--path 项目目录…"，
/// 刚给过一个路径的人只会想"我不是已经给了吗"，然后卡在同一条命令上。
#[test]
fn a_path_used_as_a_selector_is_told_apart_from_dash_dash_path() {
    let env = Env::new();
    env.ok(&["ws", "add", ".", "--name", "one"]);
    let path = env.project.path().to_str().unwrap().to_string();

    for args in [
        vec!["upgrade", "--workspace", path.as_str()],
        vec!["upgrade", path.as_str()],
    ] {
        let output = env.gld(&args);
        assert!(!output.status.success(), "{args:?} 该失败：什么都没让它改");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("是在挑要改哪个工作区"),
            "{args:?} 没解释那个路径的作用：{stderr}"
        );
        assert!(
            stderr.contains(&format!("--path {path}")),
            "{args:?} 没给出真要换目录时该怎么写：{stderr}"
        );
    }

    // 选择器不像路径时不该多这一句——`-w one` 本来就没有歧义。
    let stderr = String::from_utf8_lossy(&env.gld(&["upgrade", "-w", "one"]).stderr).into_owned();
    assert!(stderr.contains("没说要改什么"), "{stderr}");
    assert!(
        !stderr.contains("是在挑要改哪个工作区"),
        "名称没有歧义，不该多解释一句：{stderr}"
    );
}

/// `gld start <目录> --tunnel <地址>`：登记、配公网入口、启动、报连接信息，一步到位。
#[test]
fn start_with_a_tunnel_lands_on_a_usable_public_endpoint() {
    let env = Env::new();
    let text = env.ok(&[
        "start",
        env.project.path().to_str().unwrap(),
        "--port",
        &free_port().to_string(),
        "--tunnel",
        "https://mcp.example.com/mcp",
    ]);

    // 起完直接给连接信息，不用再敲一条 ls。
    assert!(
        text.contains("https://mcp.example.com/mcp"),
        "没打印公网地址：{text}"
    );
    assert!(text.contains("认证方式"), "没打印连接信息：{text}");
    assert_eq!(
        env.json(&["--json", "list"])["mcp"]["public_url"],
        "https://mcp.example.com/mcp"
    );
}

/// `gld list`：属于某个工作区时给详情，否则列出全部。
#[test]
fn ls_shows_one_workspace_in_detail_and_many_as_a_list() {
    let env = Env::new();
    env.ok(&[
        "start",
        env.project.path().to_str().unwrap(),
        "--port",
        &free_port().to_string(),
    ]);

    let detail = env.ok(&["list"]);
    assert!(detail.contains("MCP（ChatGPT 连接器"), "{detail}");
    // 隧道信息是这次要求补上的：没配也要说清楚"没有公网入口"。
    assert!(detail.contains("隧道"), "详情里没有隧道那一行：{detail}");

    // --all 在工作区目录里也强制给列表。
    let list = env.ok(&["list", "--all"]);
    assert!(list.contains("工作区") && list.contains("隧道"), "{list}");
    assert!(
        env.json(&["--json", "list", "--all"]).is_array(),
        "--json --all 该是数组"
    );

    // ls 是同一条命令的别名，敲惯了的人不该撞上 "unrecognized subcommand"。
    assert_eq!(env.ok(&["ls"]), detail);
}

/// `gld upgrade --tunnel`：换地址，并且把旧值清干净。
#[test]
fn upgrade_replaces_the_public_entry_point() {
    let env = Env::new();
    env.ok(&[
        "start",
        env.project.path().to_str().unwrap(),
        "--port",
        &free_port().to_string(),
        "--tunnel",
        "https://old.example.com/mcp",
    ]);

    env.ok(&["upgrade", "--tunnel", "https://new.example.com/mcp"]);
    assert_eq!(
        env.json(&["--json", "list"])["mcp"]["public_url"],
        "https://new.example.com/mcp"
    );

    env.ok(&["upgrade", "--off"]);
    assert_eq!(env.json(&["--json", "list"])["mcp"]["public_url"], "");
}

/// `gld upgrade --path`：项目搬了目录，服务要跟着搬。
///
/// 光把配置里的路径改掉是不够的——正在跑的服务持有的还是旧目录，
/// 表现是 `gld list` 显示新路径，而 Agent 读到的仍是旧仓库的文件。
#[test]
fn upgrade_moves_the_workspace_and_the_running_service_follows() {
    let env = Env::new();
    env.write("marker.txt", "OLD\n");
    env.ok(&[
        "start",
        env.project.path().to_str().unwrap(),
        "--port",
        &free_port().to_string(),
    ]);

    let moved = tempfile::tempdir().expect("moved project");
    std::fs::write(moved.path().join("marker.txt"), "NEW\n").expect("write marker");
    env.ok(&["upgrade", "--path", moved.path().to_str().unwrap()]);

    let content = env.json(&["--json", "tool", "call", "read_file", "path=marker.txt"])["content"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(content, "NEW\n", "服务还在旧目录上跑");
}

/// 同一个目录不能挂两个工作区：两行一样的路径，谁也说不清连的是哪个。
#[test]
fn upgrade_refuses_to_point_two_workspaces_at_one_directory() {
    let env = Env::new();
    env.ok(&["ws", "add", ".", "--name", "a"]);
    let other = tempfile::tempdir().expect("other");
    env.ok(&["ws", "add", other.path().to_str().unwrap(), "--name", "b"]);

    let output = env.gld(&[
        "upgrade",
        "b",
        "--path",
        env.project.path().to_str().unwrap(),
    ]);
    assert!(!output.status.success(), "两个工作区指到同一个目录了");
    assert!(String::from_utf8_lossy(&output.stderr).contains("已经是工作区"));
}

/// 什么都没给时要说清楚能改什么，而不是静默成功。
#[test]
fn upgrade_without_any_change_explains_the_options() {
    let env = Env::new();
    env.ok(&["ws", "add", ".", "--name", "idle"]);

    let output = env.gld(&["upgrade"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("没说要改什么"), "{stderr}");
    assert!(stderr.contains("--tunnel"), "{stderr}");
}
