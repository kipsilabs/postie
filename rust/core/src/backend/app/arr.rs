//! Port of `backend/arr.go`: Sonarr/Radarr/... webhook instances stored in the config.

use std::sync::Arc;

use super::App;
use crate::arr;
use crate::config::ArrInstance;

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default()
}

impl App {
    /// Registers the Postie webhook on the arr instance, then persists the instance with its webhook id.
    pub async fn setup_arr_webhook(
        self: &Arc<Self>,
        mut instance: ArrInstance,
        webhook_url: &str,
    ) -> Result<ArrInstance, String> {
        let id = arr::setup_webhook(&client(), &instance, webhook_url)
            .await
            .map_err(|e| format!("setting up arr webhook: {e}"))?;
        instance.webhook_id = id;
        instance.enabled = true;
        let mut cfg = self.get_config()?;
        cfg.arr
            .instances
            .get_or_insert_with(Vec::new)
            .push(instance.clone());
        self.save_config(cfg).await?;
        Ok(instance)
    }

    /// Removes the instance from the config; a webhook that cannot be removed remotely (unreachable arr)
    /// does not stop that.
    pub async fn remove_arr_instance(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let mut cfg = self.get_config()?;
        let list = cfg.arr.instances.get_or_insert_with(Vec::new);
        let idx = list
            .iter()
            .position(|i| i.id == id)
            .ok_or_else(|| format!("arr instance {id:?} not found"))?;
        let instance = list.remove(idx);
        if let Err(e) = arr::remove_webhook(&client(), &instance).await {
            eprintln!(
                "backend: could not remove arr webhook from remote app: instanceID={id} error={e}"
            );
        }
        self.save_config(cfg).await
    }

    pub fn get_arr_instances(&self) -> Result<Vec<ArrInstance>, String> {
        Ok(self.get_config()?.arr.instances.unwrap_or_default())
    }

    pub async fn test_arr_connection(&self, instance: &ArrInstance) -> Result<(), String> {
        arr::test_connection(&client(), instance)
            .await
            .map_err(|e| e.to_string())
    }
}
