use super::workspace_fields::resolve_frp_profile;
use super::App;
use crate::error::AppResult;
use crate::settings::GlobalGatewayConfig;

impl App {
    pub fn gateway_config(&self) -> AppResult<GlobalGatewayConfig> {
        Ok(self.settings()?.global_gateway)
    }

    pub fn set_gateway_config(&self, mut config: GlobalGatewayConfig) -> AppResult<()> {
        self.update_settings(|settings| {
            // 只在值真的变了的时候解析。命令行是"读旧配置 → 改给出的项 → 整体发回"，
            // 每次都校验的话，一个早就被删掉的 FRP 配置会连带让 `--port` 这种
            // 无关的修改一起失败，而用户根本没碰那个字段。
            if config.frp_profile_id != settings.global_gateway.frp_profile_id {
                config.frp_profile_id =
                    resolve_frp_profile(&config.frp_profile_id, &settings.frp_profiles)?;
            }
            settings.global_gateway = config;
            Ok(())
        })
    }
}
