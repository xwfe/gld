//! 握手得到的工具与参数能力快照；调用前据此拒绝对端不认识的操作。
//! 本模块只解释已有工具表，不发现服务、不建立连接、不决定调用权限。

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use crate::peer::PeerError;

/// 远端这一版 ccnm 到底有哪些工具、每个收哪些参数。
///
/// **为什么要存这个。**gld 的远端工具名单跟的是 ccnm 的最新契约，Runtime 上
/// 装的可能是旧版。旧版遇到不认识的**工具名**会明确报错，这没问题；遇到不
/// 认识的**参数**却什么都不说——ccnm 的参数结构体没开 `deny_unknown_fields`，
/// 多余的键直接被 serde 丢掉。于是 `run_in_background: true` 在旧版上等于没
/// 写，命令在前台跑满 timeout，模型拿回来的却是一个"已经在后台跑了"的结果，
/// 然后照着这个假设去调 `remote_read_output`。
///
/// 所以握手之后读一次 `tools/list`，调用前拿它核一遍：远端没有的工具、不收
/// 的参数，在这边就拒，并说明是版本的事。这也正是 RFC-0002 5.2 说的「工具
/// 允许集取……与 Runtime 实际能力的交集」。
#[derive(Debug, Clone, Default)]
pub struct Offered {
    /// 工具名 → 它的 `inputSchema.properties` 里的键。
    tools: HashMap<String, HashSet<String>>,
    /// 这份工具表是谁给的、长什么样。见 [`RemoteGeneration`]。
    generation: RemoteGeneration,
}

/// 判断依据的出处：这次拒绝是照着**对面哪一份能力**做的。
///
/// 拒绝的话一直是"那台机器上的 ccnm 太老了，升级它"，但从没说过对面到底是
/// 哪一版——于是没法判断"我升过了吗"，也没法判断"是不是连错机器了"
/// （跨仓评审 X10：源码、构建、部署与客户端可见能力要有一条可核对的链）。
///
/// 不另造版本号：**握手报的 `serverInfo` 加 `tools/list` 就是能力代次**。
/// `digest` 把工具名和各自的参数名排序后哈希，两份不同的工具表数不一样；
/// 对面什么都没报时是 `None`——说不知道，不编。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteGeneration {
    pub server_name: Option<String>,
    pub server_version: Option<String>,
    pub tool_count: usize,
    pub digest: String,
}

impl RemoteGeneration {
    /// 给人看的一行："ccnm 0.8.0"。对面没报就是 "unknown"。
    pub fn label(&self) -> String {
        match (&self.server_name, &self.server_version) {
            (Some(name), Some(version)) => format!("{name} {version}"),
            (Some(name), None) => name.clone(),
            (None, Some(version)) => format!("unknown {version}"),
            (None, None) => "unknown".to_string(),
        }
    }

    pub fn to_value(&self) -> Value {
        json!({
            "server_name": self.server_name,
            "server_version": self.server_version,
            "tool_count": self.tool_count,
            "tools_digest": self.digest
        })
    }
}

impl Offered {
    /// 从 `tools/list` 的结果里读。看不懂的条目跳过：这里宁可少记一个工具
    /// （调用时报"远端没有"），也不要凭空假设它有。
    pub fn from_tools(tools: &[Value]) -> Offered {
        Offered::from_handshake(tools, None)
    }

    /// 同上，外加握手时那份 `serverInfo`。
    pub fn from_handshake(tools: &[Value], server_info: Option<&Value>) -> Offered {
        let mut map: HashMap<String, HashSet<String>> = HashMap::new();
        for tool in tools {
            let Some(name) = tool.get("name").and_then(Value::as_str) else {
                continue;
            };
            let arguments = tool
                .get("inputSchema")
                .and_then(|schema| schema.get("properties"))
                .and_then(Value::as_object)
                .map(|properties| properties.keys().cloned().collect())
                .unwrap_or_default();
            map.insert(name.to_string(), arguments);
        }
        let digest = tools_digest(&map);
        let generation = RemoteGeneration {
            server_name: server_info
                .and_then(|info| info.get("name"))
                .and_then(Value::as_str)
                .map(str::to_string),
            server_version: server_info
                .and_then(|info| info.get("version"))
                .and_then(Value::as_str)
                .map(str::to_string),
            tool_count: map.len(),
            digest,
        };
        Offered {
            tools: map,
            generation,
        }
    }

    pub fn generation(&self) -> &RemoteGeneration {
        &self.generation
    }

    /// 这次调用能不能发出去。
    pub fn check(&self, tool: &str, arguments: &Value) -> Result<(), PeerError> {
        let Some(accepted) = self.tools.get(tool) else {
            return Err(PeerError::Unsupported {
                tool: tool.to_string(),
                argument: None,
                remote: Box::new(self.generation.clone()),
            });
        };
        let Some(given) = arguments.as_object() else {
            return Ok(());
        };
        // 按参数名排序再挑第一个：同一次调用每次都报同一个参数，错误信息
        // 才是可复现的（HashSet 的遍历顺序不是）。
        let mut unknown: Vec<&String> = given
            .keys()
            .filter(|name| !accepted.contains(name.as_str()))
            .collect();
        unknown.sort();
        match unknown.first() {
            Some(argument) => Err(PeerError::Unsupported {
                tool: tool.to_string(),
                argument: Some((*argument).clone()),
                remote: Box::new(self.generation.clone()),
            }),
            None => Ok(()),
        }
    }
}

/// 把工具表压成一个短摘要。
///
/// 排序之后再哈希：`HashMap` / `HashSet` 的遍历顺序每次进程都不一样，不排序
/// 的话同一份工具表每次算出的数都不同，那这个摘要就什么也证明不了。
fn tools_digest(tools: &HashMap<String, HashSet<String>>) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut lines: Vec<String> = tools
        .iter()
        .map(|(name, arguments)| {
            let mut arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
            arguments.sort_unstable();
            format!("{name}({})", arguments.join(","))
        })
        .collect();
    lines.sort();
    let mut hasher = DefaultHasher::new();
    lines.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 工具表摘要要稳定，不然它什么也证明不了。
    ///
    /// `HashMap` / `HashSet` 的遍历顺序每个进程都不一样，不排序就哈希的话，
    /// 同一份工具表每次算出的数都不同——读的人会以为对面换了版本。
    #[test]
    fn the_tool_digest_is_stable_for_the_same_table_and_moves_for_a_different_one() {
        let tools = |extra: &[&str]| {
            let mut properties = serde_json::Map::new();
            for name in extra {
                properties.insert((*name).to_string(), json!({ "type": "string" }));
            }
            vec![
                json!({ "name": "read_file", "inputSchema": { "properties": { "path": {} } } }),
                json!({ "name": "exec_command", "inputSchema": { "properties": properties } }),
            ]
        };
        let info = json!({ "name": "ccnm", "version": "0.8.0" });

        let a = Offered::from_handshake(&tools(&["cmd"]), Some(&info));
        let b = Offered::from_handshake(&tools(&["cmd"]), Some(&info));
        assert_eq!(
            a.generation().digest,
            b.generation().digest,
            "同一份工具表两次算出的摘要必须一样"
        );

        // 多一个参数就是另一份能力表。
        let c = Offered::from_handshake(&tools(&["cmd", "run_in_background"]), Some(&info));
        assert_ne!(a.generation().digest, c.generation().digest);
        assert_eq!(c.generation().label(), "ccnm 0.8.0");

        // 对面什么都没报时说不知道，不编一个版本号出来。
        let quiet = Offered::from_handshake(&tools(&["cmd"]), None);
        assert_eq!(quiet.generation().label(), "unknown");
        assert_eq!(quiet.generation().server_version, None);
        // 但工具表还是那一份，摘要照样对得上。
        assert_eq!(quiet.generation().digest, a.generation().digest);
    }
}
