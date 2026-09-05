use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{AppError, AppResult};
use crate::settings::AppSettings;

use super::model::{AppData, LegacyProfilesOnlyFile};

/// 数据目录根下的旧布局：`profiles.json`（只有工作区列表）+ `app_settings.json`
/// （FRP 配置、代理、密钥）两个文件分开存。现在的布局是把两者合成一份
/// `data/profiles.json`（[`AppData`]）。
///
/// 只在新文件还不存在时读一次，读完 [`maybe_backup_legacy_files`] 会把旧文件
/// 改名成 `.bak`，所以正常情况下每个数据目录只经历一次。
///
/// **删它之前先确认没有用户还停在旧布局上**：这条路径一断，那些人升级上来
/// 会被当成"还没配过"，工作区和密钥全部读不到——而密钥没有第二份副本。
const LEGACY_PROFILES_FILE: &str = "profiles.json";
const LEGACY_SETTINGS_FILE: &str = "app_settings.json";

pub fn data_file_path() -> AppResult<PathBuf> {
    Ok(crate::home::data_home()?.join("data").join("profiles.json"))
}

/// 读一个已存在的数据文件。单独拆出来是为了能对着临时路径测——
/// `load_or_migrate` 走的是进程级的 `GLD_HOME`，并行测试会互相覆盖同一个文件。
fn read_data_file(path: &Path) -> AppResult<AppData> {
    let raw = fs::read_to_string(path)?;
    serde_json::from_str(&raw).map_err(|error| corrupt_data_file(path, &error))
}

pub fn load_or_migrate() -> AppResult<AppData> {
    let path = data_file_path()?;
    if path.exists() {
        return read_data_file(&path);
    }

    let app_root = crate::home::data_home()?;
    let mut data = AppData::default();

    let legacy_profiles = app_root.join(LEGACY_PROFILES_FILE);
    if legacy_profiles.exists() {
        let raw = fs::read_to_string(&legacy_profiles)?;
        if let Ok(file) = serde_json::from_str::<LegacyProfilesOnlyFile>(&raw) {
            data.profiles = file.profiles;
        }
    }

    let legacy_settings = app_root.join(LEGACY_SETTINGS_FILE);
    if legacy_settings.exists() {
        let raw = fs::read_to_string(&legacy_settings)?;
        if let Ok(settings) = serde_json::from_str::<AppSettings>(&raw) {
            merge_settings(&mut data, settings);
        }
    }

    Ok(data)
}

/// 数据文件读不出来时，宁可整个 gld 停机，也不能当成"还没配过"。
///
/// 这里以前是 `unwrap_or_default()`。后果是：`profiles.json` 只要坏一个字节
/// （断电写了一半、手工编辑手滑、从坏备份还原），`gld ws list` 就回一句
/// "还没有工作区"，然后**下一条会写盘的命令把空白配置存回去**——所有工作区、
/// 所有 bearer_token / oauth_password / actions_api_key 一起没了，还没有任何提示。
/// 而这些密钥是随机生成、只存在这一个文件里的，丢了就只能把每个 ChatGPT
/// 连接器重配一遍。
///
/// 所以这里不自动改名、不自动重建：坏文件原地保留，让人自己决定是恢复备份
/// 还是从头来。自动"修好"等于把数据丢失藏起来。
fn corrupt_data_file(path: &Path, error: &serde_json::Error) -> AppError {
    AppError::Message(format!(
        "数据文件解析失败：{}\n  {error}\n\
         这个文件里存着所有工作区配置和密钥，gld 不会覆盖它。请三选一：\n\
         1) 有备份就还原备份；\n\
         2) 用编辑器把 JSON 改回合法（多半是文件被截断，末尾缺 }}）；\n\
         3) 确认不要这些数据了，先 mv \"{0}\" \"{0}.bad\" 再重新 gld ws add。",
        path.display()
    ))
}

pub fn save(data: &AppData) -> AppResult<()> {
    let path = data_file_path()?;
    write_data(&path, data)
}

fn write_data(path: &Path, data: &AppData) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        crate::home::create_data_dir(parent)?;
    }
    let text = serde_json::to_string_pretty(data)?;
    // 先写临时文件再改名：进程在写到一半时被杀，不会留下半个 JSON。
    // 临时文件名带上 pid，否则同一个 GLD_HOME 下的两个写入方会抢同一个 .tmp，
    // 一方 rename 走了，另一方的 rename 就报 NotFound。
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    fs::write(&tmp, format!("{text}\n"))?;
    // 文件里有密钥明文，只允许当前用户读写。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn maybe_backup_legacy_files(path: &Path) -> AppResult<()> {
    if !path.exists() {
        return Ok(());
    }
    let app_root = crate::home::data_home()?;
    for name in [LEGACY_PROFILES_FILE, LEGACY_SETTINGS_FILE] {
        let legacy = app_root.join(name);
        if legacy.exists() {
            let backup = app_root.join(format!("{name}.bak"));
            if !backup.exists() {
                let _ = fs::rename(&legacy, &backup);
            }
        }
    }
    Ok(())
}

fn merge_settings(data: &mut AppData, settings: AppSettings) {
    data.frp_profiles = settings.frp_profiles;
    data.last_workspace_id = settings.last_workspace_id;
    data.proxy = settings.proxy;
    data.shared_secrets = settings.shared_secrets;
    data.workspace_secrets = settings.workspace_secrets;
    data.app_secrets = settings.app_secrets;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 半个 JSON 必须报错，不能当成空配置——否则下一次 save 就把密钥抹了。
    #[test]
    fn a_truncated_data_file_is_an_error_not_an_empty_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("profiles.json");
        let mut data = AppData::default();
        data.workspace_secrets
            .entry("ws".into())
            .or_default()
            .insert("bearer_token".into(), "keep-me".into());
        write_data(&path, &data).expect("write");

        let raw = fs::read_to_string(&path).expect("read");
        fs::write(&path, &raw[..raw.len() / 2]).expect("truncate");

        let text = read_data_file(&path)
            .expect_err("坏文件必须报错")
            .to_string();
        assert!(text.contains("数据文件解析失败"), "{text}");
        // 报错要能直接照着做，而不是只说"失败了"。
        assert!(text.contains("还原备份"), "{text}");
        assert!(text.contains(&path.display().to_string()), "{text}");
    }

    /// 反向断言：合法文件仍然能读，别把修复做成"一律报错"。
    #[test]
    fn a_valid_data_file_still_loads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("profiles.json");
        let data = AppData {
            last_workspace_id: "abc".into(),
            ..AppData::default()
        };
        write_data(&path, &data).expect("write");

        assert_eq!(
            read_data_file(&path).expect("load").last_workspace_id,
            "abc"
        );
    }

    /// 空文件也是"写了一半"的典型形态，同样不能当成空配置。
    #[test]
    fn an_empty_data_file_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("profiles.json");
        fs::write(&path, "").expect("write");

        assert!(read_data_file(&path).is_err());
    }
}
