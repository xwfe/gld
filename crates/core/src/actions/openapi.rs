use serde_json::{json, Map, Value};

use crate::tools::{is_allowed_tool, MUTATING_TOOLS};

/// 这份文档里的 `security` 决定 GPT 调用时带不带凭据。
///
/// 写漏了的后果不在本机：服务端照常 401，可本机 curl 手动带上 Key 一切正常，
/// 只有导进 GPT 才会次次调用失败，而且它不会说是缺认证。
/// 所以认证方式一旦不是 none，这里必须给出对应的 scheme。
fn security_scheme_name(auth_type: &str) -> Option<&'static str> {
    match auth_type {
        "api_key" => Some("bearerAuth"),
        "oauth" => Some("oauthAuth"),
        _ => None,
    }
}

fn security_scheme(auth_type: &str, public_base_url: &str) -> Option<Value> {
    let base = public_base_url.trim_end_matches('/');
    match auth_type {
        "api_key" => Some(json!({
            "bearerAuth": { "type": "http", "scheme": "bearer" }
        })),
        // 授权码 + PKCE，端点跟 /.well-known/oauth-authorization-server 里公布的一致。
        "oauth" => Some(json!({
            "oauthAuth": {
                "type": "oauth2",
                "flows": {
                    "authorizationCode": {
                        "authorizationUrl": format!("{base}/oauth/authorize"),
                        "tokenUrl": format!("{base}/oauth/token"),
                        "scopes": {}
                    }
                }
            }
        })),
        _ => None,
    }
}

pub fn build_openapi(tools: &[Value], public_base_url: &str, auth_type: &str) -> Value {
    let mut paths = Map::new();
    let scheme_name = security_scheme_name(auth_type);

    for tool in tools {
        let Some(name) = tool.get("name").and_then(Value::as_str) else {
            continue;
        };
        if !is_allowed_tool(name) {
            continue;
        }

        let input_schema = tool
            .get("inputSchema")
            .filter(|schema| schema.is_object())
            .cloned()
            .unwrap_or_else(|| {
                json!({
                    "type": "object",
                    "additionalProperties": true
                })
            });

        let description_raw = tool
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("Call coding tool");
        let description: String = description_raw.chars().take(700).collect();
        let summary: String = description.chars().take(300).collect();

        let mut operation = json!({
            "operationId": format!("coding_{name}"),
            "summary": summary,
            "description": description,
            "requestBody": {
                "required": false,
                "content": {
                    "application/json": {
                        "schema": input_schema
                    }
                }
            },
            "responses": {
                "200": {
                    "description": "Tool execution result",
                    "content": {
                        "application/json": {
                            "schema": { "$ref": "#/components/schemas/ToolExecutionResponse" }
                        }
                    }
                },
                "400": { "description": "Invalid request or policy rejection" },
                "401": { "description": "Missing or invalid credentials" },
                "422": { "description": "Tool execution failed" },
                "502": { "description": "MCP backend failure" }
            },
            "x-openai-isConsequential": MUTATING_TOOLS.contains(&name)
        });

        if let Some(scheme) = scheme_name {
            operation
                .as_object_mut()
                .expect("operation object")
                .insert("security".to_string(), json!([{ scheme: [] }]));
        }

        paths.insert(format!("/actions/{name}"), json!({ "post": operation }));
    }

    let mut document = json!({
        "openapi": "3.1.0",
        "info": {
            "title": "gld Actions",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "Read, modify and test a workspace through gld."
        },
        "servers": [{ "url": public_base_url.trim_end_matches('/') }],
        "paths": paths,
        "components": {
            "schemas": {
                "ContentPart": content_part_schema(),
                "ToolError": tool_error_schema(),
                "StructuredContent": structured_content_schema(),
                "ToolExecutionResponse": {
                    "type": "object",
                    "properties": {
                        "ok": { "type": "boolean" },
                        "tool": { "type": "string" },
                        "structured_content": { "$ref": "#/components/schemas/StructuredContent" },
                        "content": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/ContentPart" }
                        },
                        "is_error": { "type": "boolean" }
                    },
                    "required": ["ok", "tool", "is_error"],
                    "additionalProperties": true
                }
            }
        }
    });

    if let Some(schemes) = security_scheme(auth_type, public_base_url) {
        document
            .as_object_mut()
            .expect("document object")
            .get_mut("components")
            .and_then(Value::as_object_mut)
            .expect("components object")
            .insert("securitySchemes".to_string(), schemes);
    }

    document
}

fn content_part_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "type": { "type": "string" },
            "text": { "type": "string" },
            "mimeType": { "type": "string" },
            "data": { "type": "string" }
        },
        "required": ["type"],
        "additionalProperties": true
    })
}

fn tool_error_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "code": { "type": "string" },
            "message": { "type": "string" },
            "category": { "type": "string" },
            "retryable": { "type": "boolean" },
            "details": {
                "type": "object",
                "properties": {},
                "additionalProperties": true
            }
        },
        "additionalProperties": true
    })
}

fn structured_content_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "ok": { "type": "boolean" },
            "error": tool_error_schema(),
            "diagnostics": {
                "type": "object",
                "properties": {},
                "additionalProperties": true
            },
            "permission_request": {
                "type": "object",
                "properties": {
                    "tool_name": { "type": "string" },
                    "permission": { "type": "string" },
                    "status": { "type": "string" },
                    "retryable": { "type": "boolean" }
                },
                "additionalProperties": true
            }
        },
        "additionalProperties": true
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn openapi_without_auth_has_no_security_scheme() {
        let tools = [json!({
            "name": "read_file",
            "description": "Read a file",
            "inputSchema": { "type": "object" }
        })];
        let schema = build_openapi(&tools, "https://actions.example.com", "none");
        assert!(schema["paths"]["/actions/read_file"]["post"]["security"].is_null());
        assert!(schema["components"]["securitySchemes"].is_null());
    }

    #[test]
    fn openapi_api_key_includes_bearer_security() {
        let tools = [json!({
            "name": "read_file",
            "description": "Read a file",
            "inputSchema": { "type": "object" }
        })];
        let schema = build_openapi(&tools, "https://actions.example.com", "api_key");
        assert_eq!(
            schema["components"]["securitySchemes"]["bearerAuth"]["scheme"],
            "bearer"
        );
        assert_eq!(
            schema["paths"]["/actions/read_file"]["post"]["security"],
            json!([{ "bearerAuth": [] }])
        );
    }

    /// 认证方式是 oauth 时，文档必须写出 OAuth2 流程。
    ///
    /// 之前只认 api_key，oauth 走到 else 分支——文档里一个 security 都没有，
    /// 而服务端照样要 token。导进自定义 GPT 就是次次调用 401，
    /// 本机 curl 却一切正常，因为 curl 不看文档。
    #[test]
    fn openapi_oauth_declares_the_authorization_code_flow() {
        let tools = [json!({
            "name": "read_file",
            "description": "Read a file",
            "inputSchema": { "type": "object" }
        })];
        let schema = build_openapi(&tools, "https://actions.example.com/", "oauth");

        let flow =
            &schema["components"]["securitySchemes"]["oauthAuth"]["flows"]["authorizationCode"];
        assert_eq!(
            schema["components"]["securitySchemes"]["oauthAuth"]["type"],
            "oauth2"
        );
        assert_eq!(
            flow["authorizationUrl"],
            "https://actions.example.com/oauth/authorize"
        );
        assert_eq!(flow["tokenUrl"], "https://actions.example.com/oauth/token");
        assert_eq!(
            schema["paths"]["/actions/read_file"]["post"]["security"],
            json!([{ "oauthAuth": [] }]),
            "每个接口都要声明 security，否则 GPT 不带凭据"
        );
    }

    #[test]
    fn core_openapi_exposes_grep_text_as_read_only() {
        let tools = crate::tools::list_tools_for_profile("core");
        let schema = build_openapi(&tools, "https://actions.example.com", "none");
        let operation = &schema["paths"]["/actions/grep_text"]["post"];

        assert_eq!(operation["operationId"], "coding_grep_text");
        assert_eq!(operation["x-openai-isConsequential"], false);
        assert_eq!(
            operation["requestBody"]["content"]["application/json"]["schema"],
            crate::tools::registry::input_schema("grep_text")
        );
    }
}
