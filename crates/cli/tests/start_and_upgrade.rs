//! `gld start <目录>` / `gld ls` / `gld upgrade`：一条命令从零到能用。
//!
//! 这三条命令合起来替掉了以前的四步（`workspace add` → `start` → `connect`
//! → `ws set` + `restart`）。合并之后新出现的风险都在"到底作用在哪个项目"上：
//!
//! 1. 自动登记不能张冠李戴——在一个全新目录里敲 `gld start <目录>`，如果系统里
//!    正好只有一个项目，旧的推断规则会把那个不相干的项目拿来当成它；
//! 2. 已登记项目的子目录里不能又建一个——那样同一个仓库会有两个项目，
//!    而列表里看着像两个；
//! 3. `-w` 拼错时不能顺手建一个：那是打字错误，不是新项目；
//! 4. 只剩一个服务之后（RFC-0004），在不是项目的目录里敲 `gld start` 只起服务，
//!    不能把那个目录（比如整个主目录）登记进来。

mod common;

use common::env::{free_port, Env};

/// 服务里的项目名。
fn project_names(env: &Env) -> Vec<String> {
    env.json(&["--json", "ls"])["service"]["members"]
        .as_array()
        .expect("项目表")
        .iter()
        .map(|item| item["name"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn public_endpoint(env: &Env) -> String {
    env.json(&["--json", "ls"])["service"]["publicEndpoint"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// 目录没登记过，一条 `gld start <目录>` 就该加进来 + 起服务。
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

    assert!(text.contains("已加入项目"), "没有告知自动登记：{text}");
    assert_eq!(project_names(&env).len(), 1);
    assert_eq!(
        env.json(&["--json", "status"])["service"]["state"],
        "running",
        "服务没起来"
    );
}

/// `gld start <新目录>` 不能把碰巧唯一的那个项目拿来当成它。
///
/// 旧的项目推断有一条"只有一个项目就用它"的回退，对只读命令很方便，
/// 但套到 start 上就是：你在 ~/code/new 里启动，加进来的却是 ~/code/old，
/// 而输出里没有任何地方能看出这一点。
#[test]
fn start_in_a_new_directory_never_hijacks_the_only_existing_project() {
    let env = Env::new();
    env.ok(&["add", ".", "--name", "first"]);

    let other = tempfile::tempdir().expect("another project");
    let text = env.ok(&[
        "start",
        other.path().to_str().unwrap(),
        "--port",
        &free_port().to_string(),
    ]);
    assert!(text.contains("已加入项目"), "{text}");

    let names = project_names(&env);
    assert_eq!(names.len(), 2, "第二个目录没被加成自己的项目：{names:?}");
    assert!(names.iter().any(|name| name == "first"));
}

/// 在不是项目的目录里敲 `gld start`（已经有别的项目时）：只起服务，不登记它。
///
/// 只剩一个服务之后，`gld start` 也是"把服务拉起来"的那条命令，随手在主目录里
/// 敲一下很常见。以前这会把整个主目录登记成项目、交给 AI。
#[test]
fn start_outside_any_project_only_starts_the_service() {
    let env = Env::new();
    env.ok(&["add", ".", "--name", "first"]);
    env.ok(&["upgrade", "--port", &free_port().to_string()]);

    let elsewhere = tempfile::tempdir().expect("not a project");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_gld"))
        .args(["start"])
        .env("GLD_HOME", env.home.path())
        .env("HOME", env.user_home())
        .env("NO_COLOR", "1")
        .env_remove("GLD_WORKSPACE")
        .current_dir(elsewhere.path())
        .output()
        .expect("run gld");
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("不是项目，没加进来"), "{stderr}");
    assert!(
        stderr.contains("gld add ."),
        "要告诉人真想加时怎么加：{stderr}"
    );
    assert_eq!(project_names(&env), vec!["first"]);
    assert_eq!(
        env.json(&["--json", "status"])["service"]["state"],
        "running"
    );
}

/// 已登记项目的子目录属于那个项目，不该再建一个。
#[test]
fn start_inside_a_subdirectory_reuses_the_project_that_owns_it() {
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
        !text.contains("已加入项目"),
        "子目录被当成新项目登记了：{text}"
    );
    assert_eq!(project_names(&env).len(), 1);
}

/// 相对路径要按用户敲命令时所在的目录算，不能按守护进程的工作目录算。
///
/// 守护进程的工作目录是数据目录。以前 CLI 把 `../ccnm` 原样转发过去，守护进程
/// 就按数据目录去解析——`~/.config` 底下正好有个同名目录的话（很多工具都往那儿
/// 放配置），gld 会一声不吭地把别人的配置目录登记成项目。用户看到的现象是
/// 「同一条命令跑两次，冒出两个项目」，而第二个指着一个毫不相干的目录。
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

    env.ok(&["add", "ccnm", "--name", "proj"]);

    let list = env.json(&["--json", "ls"]);
    let registered = list["service"]["members"][0]["path"]
        .as_str()
        .expect("path")
        .to_string();
    assert_eq!(
        std::path::Path::new(&registered),
        real.canonicalize().expect("canonical project dir"),
        "登记到了别的目录去"
    );

    // selector 也一样：`gld rm ccnm` 里的 ccnm 是相对路径，
    // 名字（proj）对不上，只能靠路径匹配。
    env.ok(&["rm", "ccnm", "-y"]);
    assert_eq!(project_names(&env).len(), 0, "相对路径 selector 没匹配上");
}

/// `-w` 写了个不存在的名字 = 打字错误，要报错并列候选，而不是建一个新的。
#[test]
fn a_typo_in_dash_w_is_an_error_not_a_new_project() {
    let env = Env::new();
    env.ok(&["add", ".", "--name", "real"]);

    let output = env.gld(&["start", "-w", "rael"]);
    assert!(!output.status.success(), "拼错的项目名被接受了");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("未找到项目"), "{stderr}");
    assert_eq!(project_names(&env).len(), 1, "拼错名字不该多出一个项目");
}

/// 目录和 `-w` 同时给出时意图矛盾，要当场问清楚而不是挑一个。
#[test]
fn a_path_and_dash_w_together_are_refused() {
    let env = Env::new();
    env.ok(&["add", ".", "--name", "one"]);

    let output = env.gld(&["start", env.project.path().to_str().unwrap(), "-w", "one"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("不知道该听哪个"));
}

/// 用路径挑了项目却没说要改什么时，得先讲清那个路径是干什么用的。
///
/// `-w <路径>` 和 `--path <路径>` 都收路径，方向却相反：一个是"改哪个项目"，
/// 一个是"把目录改成这个"。光回一句"没说要改什么。可改的：--path 目录…"，
/// 刚给过一个路径的人只会想"我不是已经给了吗"，然后卡在同一条命令上。
#[test]
fn a_path_used_as_a_selector_is_told_apart_from_dash_dash_path() {
    let env = Env::new();
    env.ok(&["add", ".", "--name", "one"]);
    let path = env.project.path().to_str().unwrap().to_string();

    for args in [
        vec!["upgrade", "--workspace", path.as_str()],
        vec!["upgrade", path.as_str()],
    ] {
        let output = env.gld(&args);
        assert!(!output.status.success(), "{args:?} 该失败：什么都没让它改");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("是在挑要改哪个项目"),
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
        !stderr.contains("是在挑要改哪个项目"),
        "名称没有歧义，不该多解释一句：{stderr}"
    );
}

/// `gld start <目录> --tunnel <地址>`：加项目、配公网入口、启动、报连接信息，一步到位。
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
    assert_eq!(public_endpoint(&env), "https://mcp.example.com/mcp");
}

/// `gld ls`：服务的连接信息和项目表；`gld ls <项目>` 看那个项目的配置。
#[test]
fn ls_shows_the_service_and_one_project_in_detail() {
    let env = Env::new();
    env.ok(&[
        "start",
        env.project.path().to_str().unwrap(),
        "--port",
        &free_port().to_string(),
    ]);
    let name = project_names(&env).remove(0);

    let overview = env.ok(&["ls"]);
    assert!(overview.contains("MCP 服务"), "{overview}");
    // 公网入口是这次要求补上的：没配也要说清楚"没有"。
    assert!(overview.contains("公网入口"), "{overview}");
    assert!(overview.contains(&name), "项目表里没有它：{overview}");

    let detail = env.ok(&["ls", &name]);
    assert!(
        detail.contains("在服务里") && detail.contains("工具集"),
        "{detail}"
    );

    // list 是同一条命令，老写法 `--all` 也还认。
    assert_eq!(env.ok(&["list"]), overview);
    assert_eq!(env.ok(&["ls", "--all"]), overview);
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
    assert_eq!(public_endpoint(&env), "https://new.example.com/mcp");

    env.ok(&["upgrade", "--off"]);
    assert_eq!(public_endpoint(&env), "");
}

/// `gld upgrade --path`：项目搬了目录，服务要跟着搬。
///
/// 光把配置里的路径改掉是不够的——服务持有的要是旧目录，表现是 `gld ls`
/// 显示新路径，而 AI 读到的仍是旧仓库的文件。
#[test]
fn upgrade_moves_the_project_and_the_running_service_follows() {
    let env = Env::new();
    env.write("marker.txt", "OLD\n");
    let service = env.serve("mover", "noauth");

    let moved = tempfile::tempdir().expect("moved project");
    std::fs::write(moved.path().join("marker.txt"), "NEW\n").expect("write marker");
    env.ok(&["upgrade", "mover", "--path", moved.path().to_str().unwrap()]);

    let read = service.call_tool("read_file", serde_json::json!({ "path": "marker.txt" }));
    assert_eq!(read["content"], "NEW\n", "服务还在旧目录上跑：{read}");
}

/// 同一个目录不能挂两个项目：两行一样的路径，谁也说不清连的是哪个。
#[test]
fn upgrade_refuses_to_point_two_projects_at_one_directory() {
    let env = Env::new();
    env.ok(&["add", ".", "--name", "a"]);
    let other = tempfile::tempdir().expect("other");
    env.ok(&["add", other.path().to_str().unwrap(), "--name", "b"]);

    let output = env.gld(&[
        "upgrade",
        "b",
        "--path",
        env.project.path().to_str().unwrap(),
    ]);
    assert!(!output.status.success(), "两个项目指到同一个目录了");
    assert!(String::from_utf8_lossy(&output.stderr).contains("已经是项目"));
}

/// 什么都没给时要说清楚能改什么，而不是静默成功。
#[test]
fn upgrade_without_any_change_explains_the_options() {
    let env = Env::new();
    env.ok(&["add", ".", "--name", "idle"]);

    let output = env.gld(&["upgrade"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("没说要改什么"), "{stderr}");
    assert!(stderr.contains("--tunnel"), "{stderr}");
}
