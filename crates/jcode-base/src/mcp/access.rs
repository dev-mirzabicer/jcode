use super::McpServerConfig;
use anyhow::{Result, ensure};
use jcode_tool_types::delegation::Permission;
use std::collections::BTreeSet;

#[derive(Clone, Debug)]
pub struct McpAccessPolicy {
    pub permission: Permission,
    pub blocked: BTreeSet<String>,
}

impl McpAccessPolicy {
    pub fn permits(&self, name: &str, configured: Option<&McpServerConfig>) -> bool {
        !self.blocked.contains(name)
            && (self.permission == Permission::ReadWrite
                || configured.is_some_and(|config| config.read_only))
    }

    pub(crate) fn authorize(
        &self,
        name: &str,
        configured: Option<&McpServerConfig>,
        supplied: Option<&McpServerConfig>,
    ) -> Result<()> {
        ensure!(
            self.permits(name, configured),
            "MCP server '{name}' is unavailable under this child's permission or MCP blocklist"
        );
        if self.permission == Permission::ReadOnly
            && let Some(supplied) = supplied
        {
            let expected = configured.ok_or_else(|| {
                anyhow::anyhow!("Read-only MCP use requires a classified configured server")
            })?;
            ensure!(
                expected.same_launch(supplied),
                "Read-only MCP connection differs from its classified configuration"
            );
        }
        Ok(())
    }
}

impl McpServerConfig {
    /// Classification and automatic-enablement flags do not identify the process.
    /// Compare exact launch values, not an untrusted name or a cache-hint hash.
    pub(crate) fn same_launch(&self, other: &Self) -> bool {
        self.command == other.command
            && self.args == other.args
            && self.env == other.env
            && self.shared == other.shared
            && self.transport == other.transport
            && self.url == other.url
            && self.headers == other.headers
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::mcp::{McpConfig, McpManager, SharedMcpPool};
    use std::sync::Arc;

    fn server(label: &str, read_only: bool) -> McpServerConfig {
        let script = r#"import sys,json,os
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r: continue
 m=r.get('method')
 if m=='initialize': result={'protocolVersion':'2024-11-05','capabilities':{'tools':{}},'serverInfo':{'name':'fixture','version':'1'}}
 elif m=='tools/list': result={'tools':[{'name':'identify','description':'synthetic','inputSchema':{'type':'object','properties':{}}}]}
 elif m=='tools/call': result={'content':[{'type':'text','text':json.dumps({'label':os.environ['LABEL'],'pid':os.getpid(),'cwd':os.getcwd()})}]}
 else: result={}
 print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#;
        serde_json::from_value(serde_json::json!({"command":"python3","args":["-u","-c",script],"env":{"LABEL":label},"shared":true,"read_only":read_only})).unwrap()
    }

    async fn identity(manager: &McpManager) -> serde_json::Value {
        let response = manager
            .call_tool("fixture", "identify", serde_json::json!({}))
            .await
            .unwrap();
        let super::super::ContentBlock::Text { text } = &response.content[0] else {
            panic!("text result");
        };
        serde_json::from_str(text).unwrap()
    }

    #[tokio::test]
    async fn mcp_policy_uses_effective_project_classification_and_exact_pooled_definition() {
        let home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
        let a = home.root().join("a");
        let b = home.root().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(
            home.root().join("mcp.json"),
            serde_json::json!({"servers":{"fixture":server("GLOBAL",false)}}).to_string(),
        )
        .unwrap();
        std::fs::write(
            b.join(".mcp.json"),
            serde_json::json!({"servers":{"fixture":server("PROJECT",true)}}).to_string(),
        )
        .unwrap();
        let pool = Arc::new(SharedMcpPool::new(McpConfig::load_for_dir(Some(&a))));
        let mut parent =
            McpManager::with_shared_pool_for_dir(pool.clone(), "parent".into(), Some(a.clone()));
        let child =
            McpManager::with_shared_pool_for_dir(pool.clone(), "child".into(), Some(b.clone()))
                .with_access_policy(McpAccessPolicy {
                    permission: Permission::ReadOnly,
                    blocked: Default::default(),
                });
        assert_eq!(parent.connect_all().await.unwrap().0, 1);
        assert_eq!(child.connect_all().await.unwrap().0, 1);
        let p = identity(&parent).await;
        let c = identity(&child).await;
        assert_eq!(p["label"], "GLOBAL");
        assert_eq!(c["label"], "PROJECT");
        assert_ne!(p["pid"], c["pid"]);
        assert_eq!(c["cwd"], b.canonicalize().unwrap().to_str().unwrap());
        let other =
            McpManager::with_shared_pool_for_dir(pool.clone(), "same-definition".into(), Some(b));
        other.connect_all().await.unwrap();
        assert_eq!(identity(&other).await["pid"], c["pid"]);
        parent.reload().await.unwrap();
        assert_eq!(
            identity(&child).await,
            c,
            "session reload cannot kill an unrelated shared connection"
        );
        assert!(
            child
                .connect("fixture", &server("SPOOF", true))
                .await
                .is_err()
        );
        other.disconnect_all().await;
        child.disconnect_all().await;
        parent.disconnect_all().await;
        assert!(pool.ref_counts().await.values().all(|count| *count == 0));
        pool.disconnect_all().await;
    }

    #[tokio::test]
    async fn unclassified_and_blocked_mcps_fail_before_connect_and_direct_call() {
        let _home = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
        let unclassified: McpServerConfig =
            serde_json::from_value(serde_json::json!({"command":"/nonexistent/mcp-fixture"}))
                .unwrap();
        assert!(!unclassified.read_only);
        let mut config = McpConfig::default();
        config
            .servers
            .insert("unclassified".into(), unclassified.clone());
        config
            .servers
            .insert("blocked".into(), server("BLOCKED", true));
        let manager = McpManager::with_config(config.clone()).with_access_policy(McpAccessPolicy {
            permission: Permission::ReadOnly,
            blocked: std::collections::BTreeSet::from(["blocked".into()]),
        });
        assert_eq!(manager.connect_all().await.unwrap(), (0, vec![]));
        for name in ["unclassified", "blocked", "unknown"] {
            assert!(
                manager
                    .call_tool(name, "identify", serde_json::json!({}))
                    .await
                    .is_err()
            );
            assert!(manager.connect(name, &server("SPOOF", true)).await.is_err());
        }
        let policy = McpAccessPolicy {
            permission: Permission::ReadWrite,
            blocked: std::collections::BTreeSet::from(["blocked".into()]),
        };
        assert!(policy.permits("unclassified", Some(&unclassified)));
        assert!(!policy.permits("blocked", config.servers.get("blocked")));
        assert!(
            McpManager::with_config(config).server_is_allowed("blocked"),
            "ordinary primary discovery is unchanged"
        );
    }
}
