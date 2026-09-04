use super::App;
use crate::error::AppResult;
use crate::global_gateway::{self, GatewayHealthItem, GlobalGatewayStatusDto};
use crate::settings::GlobalGatewayConfig;

impl App {
    pub fn gateway_config(&self) -> AppResult<GlobalGatewayConfig> {
        Ok(self.settings()?.global_gateway)
    }

    pub fn set_gateway_config(&self, config: GlobalGatewayConfig) -> AppResult<()> {
        self.update_settings(|settings| {
            settings.global_gateway = config;
            Ok(())
        })
    }

    pub async fn start_gateway(&self) -> AppResult<GlobalGatewayStatusDto> {
        global_gateway::ensure_started().await
    }

    pub async fn stop_gateway(&self) -> AppResult<()> {
        global_gateway::stop().await
    }

    pub async fn gateway_status(&self) -> GlobalGatewayStatusDto {
        global_gateway::status().await
    }

    pub async fn gateway_health(&self) -> Vec<GatewayHealthItem> {
        global_gateway::health().await
    }
}
