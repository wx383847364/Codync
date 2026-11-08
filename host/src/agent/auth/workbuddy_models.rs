//! `WorkBuddy` 的标准 ACP 可能只返回随包快照；通过其官方配置接口读取当前账号的目录。

use crate::agent::{acp::Acp, backends};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::time::Duration;

/// 使用 CLI 自己解密和刷新的凭据查询目录，不读取或记录磁盘上的令牌。
pub(super) async fn refresh(acp: &Acp, session: &mut Value, env: &[(String, String)]) -> Result<()> {
    let product = backends::workbuddy_product(None)?;
    ensure_default_environment(env, &product).await?;
    let info = acp.request("_codebuddy.ai/getUserInfo", json!({})).await?;
    let user = &info["userInfo"];
    let token = crate::agent::acp::codebuddy::access_token(&info)
        .context("Sign in to WorkBuddy from Codync to load the current model catalog")?;
    let url = config_url(&product)?;
    let response = fetch_catalog(url, &product, user, token).await?;
    apply_catalog(session, &response, &product)
}

/// 上游没有公开有效站点查询接口；存在覆盖时拒绝猜测，避免把账号令牌发送到错误站点。
async fn ensure_default_environment(env: &[(String, String)], product: &Value) -> Result<()> {
    const KEYS: &[&str] = &[
        "CODEBUDDY_HOME",
        "CODEBUDDY_CONFIG_DIR",
        "WORKBUDDY_CONFIG_DIR",
        "CODEBUDDY_BASE_URL",
        "CODEBUDDY_INTERNET_ENVIRONMENT",
        "CODEBUDDY_INTERNET_ENVIROMENT",
        "ACC_PRODUCT_CONFIG",
        "ACC_PRODUCT_CONFIG_V3",
        "ACC_PRODUCT_CONFIG_V2",
        "ACC_PRODUCT_CONFIG_PATH",
    ];
    for key in KEYS {
        // CLI 同时继承进程环境及 Codync 保存的 Agent 环境。
        if std::env::var_os(key).is_some() || env.iter().any(|(name, _)| name == key) {
            bail!("WorkBuddy model discovery does not support endpoint/environment overrides");
        }
    }
    if let Some(home) = dirs::home_dir() {
        for folder in [".workbuddy", ".codebuddy"] {
            let path = home.join(folder).join("settings.json");
            let text = match tokio::fs::read_to_string(path).await {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => "{}".to_owned(),
                Err(error) => return Err(error.into()),
            };
            check_cached_origin(&home.join(folder), product).await?;
            let settings: Value = serde_json::from_str(&text).context("reading WorkBuddy endpoint settings")?;
            // 保存的站点、企业地址、路由及环境覆盖均由上游解析，本补丁不复制其优先级规则。
            if ["endpoint", "enterpriseEndpoint", "envRouteMode"].iter().any(|key| !settings[key].is_null())
                || KEYS.iter().any(|key| !settings["env"][key].is_null())
            {
                bail!("WorkBuddy model discovery does not support endpoint/environment overrides");
            }
        }
    }
    Ok(())
}

/// 核对 CLI 启动后自行恢复的非敏感路由缓存，覆盖父进程看不到的站点切换。
async fn check_cached_origin(home: &std::path::Path, product: &Value) -> Result<()> {
    // 上游 FileLocalStorage 使用 entry_<MD5(键名)>.info，仅读取两个路由键。
    let entries = [
        ("entry_933d5543e80177622c17a73869c0fad7.info", false), // CodeBuddy-Endpoint-Cache
        ("entry_3bab4ce61838088127d444e4cc042d6d.info", true),  // CodeBuddy-Environment-Cache
    ];
    let expected = config_url(product)?;
    for (file, environment) in entries {
        let text = match tokio::fs::read_to_string(home.join("local_storage").join(file)).await {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let value: Value = serde_json::from_str(&text).context("reading WorkBuddy routing cache")?;
        let Some(value) = value.as_str().filter(|value| !value.is_empty()) else { continue };
        let cached = if environment { backends::workbuddy_product(Some(value))? } else { json!({"endpoint": value}) };
        if config_url(&cached)? != expected {
            bail!("WorkBuddy model discovery does not support a cached alternate endpoint");
        }
    }
    Ok(())
}

/// 请求官方目录；地址在生产入口校验，测试可使用本地 HTTP 服务核验鉴权与失败行为。
async fn fetch_catalog(url: reqwest::Url, product: &Value, user: &Value, token: &str) -> Result<Value> {
    // 禁止重定向，防止账号令牌随产品配置中的地址被发送到其他站点。
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build()?;
    let mut request = client
        .get(url)
        .bearer_auth(token)
        .timeout(Duration::from_secs(15))
        .header("X-Product", product["deploymentType"].as_str().unwrap_or("SaaS"));
    for (field, header) in [("userId", "X-User-Id"), ("enterpriseId", "X-Enterprise-Id")] {
        if let Some(value) = user[field].as_str() {
            request = request.header(header, value);
        }
    }
    let response: Value =
        request.send().await.context("loading WorkBuddy model catalog")?.error_for_status()?.json().await?;
    Ok(response)
}

/// 仅允许当前安装对应的官方 HTTPS 站点；不向自定义产品地址发送账号凭据。
fn config_url(product: &Value) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(product["endpoint"].as_str().context("missing WorkBuddy endpoint")?)?;
    if url.scheme() != "https"
        || !matches!(
            url.host_str(),
            Some("www.workbuddy.cn" | "www.workbuddy.ai" | "www.codebuddy.cn" | "www.codebuddy.ai")
        )
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        bail!("unsupported WorkBuddy model catalog endpoint");
    }
    Ok(url.join("/v3/config")?)
}

/// 按官方 `AgentManager` 的选择规则解析云端目录，并保留本机自定义模型。
fn apply_catalog(session: &mut Value, response: &Value, installed: &Value) -> Result<()> {
    if response["code"] != 0 {
        bail!("WorkBuddy model catalog request failed; check your sign-in");
    }
    let product = &response["data"];
    let remote = product["models"]
        .as_array()
        .filter(|models| !models.is_empty())
        .context("WorkBuddy did not return a current model catalog")?;
    let mut definitions = installed["models"].as_array().cloned().unwrap_or_default();
    for model in remote {
        // 云端覆盖同 ID 的安装包定义，保留默认别名等只在安装包出现的定义。
        definitions.retain(|old| old["id"] != model["id"]);
        definitions.push(model.clone());
    }
    // 云端可只更新模型定义，未更新的 cli 范围沿用安装包配置。
    let agent = product["agents"]
        .as_array()
        .and_then(|agents| agents.iter().find(|agent| agent["name"] == "cli"))
        // 云端可只更新其他代理，缺省的 cli 仍使用安装包范围。
        .or_else(|| {
            installed["agents"].as_array().and_then(|agents| agents.iter().find(|agent| agent["name"] == "cli"))
        })
        .context("WorkBuddy did not return a CLI model scope")?;
    let mut models = select_models(agent, &definitions);
    // ACP 未标记本地模型来源；保守保留安装包之外的已报告项，以免删除用户配置。
    for model in session["models"]["availableModels"].as_array().into_iter().flatten() {
        let id = &model["modelId"];
        if !installed["models"].as_array().into_iter().flatten().any(|known| known["id"] == *id)
            && !models.iter().any(|known| known["modelId"] == *id)
            // 服务端明确禁用的模型不能作为本地扩展重新加入。
            && !remote.iter().any(|known| known["id"] == *id && known["disabled"] == true)
        {
            models.push(model.clone());
        }
    }
    if models.is_empty() {
        bail!("WorkBuddy returned an empty CLI model catalog");
    }
    // 同步两个 ACP 表示，避免现有解析优先读取 configOptions 时再次取到旧快照。
    if let Some(options) = session["configOptions"].as_array_mut() {
        for option in options {
            if option["category"] == "model" || matches!(option["id"].as_str(), Some("model" | "models")) {
                // 只映射展示字段；令牌及产品配置不会进入客户端响应。
                option["options"] =
                    json!(models.iter().map(|m| json!({"value": m["modelId"], "name": m["name"]})).collect::<Vec<_>>());
            }
        }
    }
    session["models"]["availableModels"] = json!(models);
    Ok(())
}

/// 复用上游的 ID、名称、别名解析和 chat/custom 自动追加规则，排除已禁用模型。
fn select_models(agent: &Value, definitions: &[Value]) -> Vec<Value> {
    let mut selected = Vec::new();
    for id in agent["models"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        // ID 优先于显示名称和别名，避免重名定义改变模型选择。
        let model = definitions.iter().find(|m| m["id"] == id).or_else(|| {
            definitions.iter().find(|m| {
                m["name"] == id
                    || m["aliases"].as_array().is_some_and(|aliases| aliases.iter().any(|alias| alias == id))
            })
        });
        if let Some(model) = model {
            selected.push(model);
        }
    }
    // 官方会将 chat/custom 标签模型追加给主代理，最新模型不一定出现在 cli.models。
    selected.extend(definitions.iter().filter(|model| {
        model["id"].as_str().is_some_and(|id| id.starts_with("custom:"))
            || model["tags"].as_array().is_some_and(|tags| tags.iter().any(|tag| tag == "chat" || tag == "custom"))
    }));
    let mut models: Vec<Value> = Vec::new();
    for model in selected {
        let Some(id) = model["id"].as_str() else { continue };
        // 禁用标记和重复别名不能产生额外的可选项。
        if model["disabled"] == true || models.iter().any(|known| known["modelId"] == id) {
            continue;
        }
        models.push(json!({"modelId": id, "name": model["name"].as_str().unwrap_or(id)}));
    }
    models
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 云端目录替换旧列表，并且不暴露未分配给 cli 的模型。
    #[test]
    fn live_catalog_replaces_both_acp_lists() {
        let mut session = json!({"models": {"currentModelId": "auto", "availableModels": []},
            "configOptions": [{"category": "model", "currentValue": "auto", "options": [{"value": "old"}]}]});
        apply_catalog(
            &mut session,
            &json!({"code": 0, "data": {
            "agents": [{"name": "cli", "models": ["auto", "new", "new"]}],
            "models": [{"id": "auto"}, {"id": "new", "name": "New Model"}, {"id": "hidden"}]}}),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(session["models"]["availableModels"].as_array().unwrap().len(), 2);
        assert_eq!(session["configOptions"][0]["options"][1], json!({"value": "new", "name": "New Model"}));
        assert_eq!(session["models"]["currentModelId"], "auto");
    }

    /// 云端 chat 标签新模型、别名和本地自定义模型均可保留，禁用模型被排除。
    #[test]
    fn chat_models_aliases_and_local_custom_models_survive_refresh() {
        let mut session = json!({"models": {"availableModels": [{"modelId": "custom:mine", "name": "Mine"}]}});
        apply_catalog(
            &mut session,
            &json!({"code": 0, "data": {
            "agents": [{"name": "cli", "models": ["legacy-alias"]}],
            "models": [{"id": "base", "aliases": ["legacy-alias"]},
                {"id": "new", "tags": ["chat"]}, {"id": "off", "tags": ["chat"], "disabled": true},
                {"id": "image", "tags": ["text-to-image"]}]}}),
            &json!({"models": [{"id": "old"}]}),
        )
        .unwrap();
        let models = session["models"]["availableModels"].as_array().unwrap();
        assert_eq!(
            models.iter().map(|model| model["modelId"].as_str().unwrap()).collect::<Vec<_>>(),
            ["base", "new", "custom:mine"]
        );
    }

    /// 请求失败或返回空目录时保持原会话，不能把旧快照标成刷新成功。
    #[test]
    fn failed_catalog_does_not_mutate_session() {
        let original = json!({"models": {"availableModels": [{"modelId": "old"}]}});
        for response in [
            json!({"code": 401}),
            json!({"code": 0, "data": {"agents": [{"name": "cli", "models": []}], "models": []}}),
        ] {
            let mut session = original.clone();
            assert!(apply_catalog(&mut session, &response, &Value::Null).is_err());
            assert_eq!(session, original);
        }
    }

    /// 云端部分更新保留原 cli 范围，但明确禁用的 ACP 旧项不能复活。
    #[test]
    fn partial_cloud_update_keeps_cli_scope_and_drops_disabled_extras() {
        let installed = json!({"agents": [{"name": "cli", "models": ["base"]}], "models": [{"id": "base"}]});
        for agents in [json!([]), json!([{"name": "other"}])] {
            let mut session = json!({"models": {"availableModels": [{"modelId": "removed"}]}});
            apply_catalog(
                &mut session,
                &json!({"code": 0, "data": {"agents": agents,
                "models": [{"id": "removed", "disabled": true}, {"id": "latest", "tags": ["chat"]}]}}),
                &installed,
            )
            .unwrap();
            assert_eq!(
                session["models"]["availableModels"],
                json!([{"modelId": "base", "name": "base"}, {"modelId": "latest", "name": "latest"}])
            );
        }
    }

    /// CLI 内部恢复的其他站点必须在任何账号请求之前被拒绝。
    #[tokio::test]
    async fn cached_alternate_origin_is_rejected() {
        let home = std::env::temp_dir().join(format!("codync-workbuddy-route-{}", uuid::Uuid::new_v4()));
        let storage = home.join("local_storage");
        tokio::fs::create_dir_all(&storage).await.unwrap();
        let path = storage.join("entry_933d5543e80177622c17a73869c0fad7.info");
        let product = json!({"endpoint": "https://www.workbuddy.cn"});
        tokio::fs::write(&path, json!("https://www.workbuddy.ai").to_string()).await.unwrap();
        assert!(check_cached_origin(&home, &product).await.is_err());
        tokio::fs::write(&path, json!("https://www.workbuddy.cn").to_string()).await.unwrap();
        assert!(check_cached_origin(&home, &product).await.is_ok());
        tokio::fs::remove_dir_all(home).await.unwrap();
    }

    /// 实际 HTTP 请求携带账号凭据，并拒绝重定向到其他地址。
    #[tokio::test]
    async fn authenticated_catalog_request_and_redirect_rejection() {
        use axum::{
            Json, Router,
            http::{HeaderMap, StatusCode},
            routing::get,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        // 测试服务验证请求头，仅使用虚构凭据。
        let app = Router::new()
            .route(
                "/v3/config",
                get(|headers: HeaderMap| async move {
                    assert_eq!(headers["authorization"], "Bearer test-token");
                    assert_eq!(headers["x-user-id"], "test-user");
                    assert_eq!(headers["x-product"], "SaaS");
                    Json(json!({"code": 0, "data": {"agents": [{"name": "cli", "models": ["new"]}], "models": [{"id": "new", "name": "Latest"}]}}))
                }),
            )
            .route("/redirect", get(|| async { (StatusCode::FOUND, [("location", "https://example.invalid")]) }));
        // 本地服务随测试结束显式停止，不保留后台任务。
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let product = json!({"deploymentType": "SaaS"});
        let user = json!({"userId": "test-user"});
        let result =
            fetch_catalog(reqwest::Url::parse(&format!("{base}/v3/config")).unwrap(), &product, &user, "test-token")
                .await
                .unwrap();
        assert_eq!(result["data"]["models"][0]["id"], "new");
        let mut session = json!({"models": {"availableModels": [{"modelId": "old"}]},
            "configOptions": [{"id": "model", "options": [{"value": "old"}]}]});
        apply_catalog(&mut session, &result, &json!({"models": [{"id": "old"}]})).unwrap();
        assert_eq!(session["configOptions"][0]["options"], json!([{"value": "new", "name": "Latest"}]));
        assert!(
            fetch_catalog(reqwest::Url::parse(&format!("{base}/redirect")).unwrap(), &product, &user, "test-token")
                .await
                .is_err()
        );
        server.abort();
    }

    /// 拒绝外部站点、明文连接及 URL 内嵌凭据。
    #[test]
    fn credentials_only_go_to_official_origins() {
        for endpoint in [
            "https://evil.test",
            "http://www.workbuddy.cn",
            "https://user@www.workbuddy.cn",
            "https://www.workbuddy.cn:444",
        ] {
            assert!(config_url(&json!({"endpoint": endpoint})).is_err());
        }
        assert_eq!(
            config_url(&json!({"endpoint": "https://www.workbuddy.cn"})).unwrap().as_str(),
            "https://www.workbuddy.cn/v3/config"
        );
    }
}
