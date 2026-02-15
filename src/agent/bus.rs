use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, RwLock};
use uuid::Uuid;

/// A message sent between agents
#[derive(Debug)]
pub struct AgentMessage {
    pub id: Uuid,
    pub from: String,
    pub to: String,
    pub kind: MessageKind,
    pub payload: String,
    /// For `Delegate` messages: sender provides a oneshot channel to receive the
    /// response. The receiving agent sends its result back through this channel.
    pub response_tx: Option<oneshot::Sender<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageKind {
    /// "handle this task" — expects a response via `response_tx`
    Delegate,
    /// "here's my output" — fire-and-forget
    Result,
    /// fire-and-forget notification
    Notify,
    /// graceful stop request
    Shutdown,
}

/// Inter-agent message bus using tokio mpsc channels.
///
/// Design: the bus is intentionally "dumb" — it routes messages to registered
/// agents by name but does NOT enforce delegation rules. Callers should verify
/// permissions before sending.
pub struct AgentBus {
    senders: Arc<RwLock<HashMap<String, mpsc::Sender<AgentMessage>>>>,
}

impl AgentBus {
    pub fn new() -> Self {
        Self {
            senders: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register an agent and get its message receiver
    pub async fn register(&self, name: &str, buffer: usize) -> mpsc::Receiver<AgentMessage> {
        let (tx, rx) = mpsc::channel(buffer);
        self.senders.write().await.insert(name.to_string(), tx);
        rx
    }

    /// Unregister an agent (e.g., on shutdown)
    pub async fn unregister(&self, name: &str) {
        self.senders.write().await.remove(name);
    }

    /// Send a message to a specific agent.
    /// Returns error if the target agent is not registered.
    pub async fn send(&self, msg: AgentMessage) -> Result<()> {
        let senders = self.senders.read().await;
        let sender = senders
            .get(&msg.to)
            .ok_or_else(|| anyhow::anyhow!("Agent '{}' not registered on bus", msg.to))?;
        sender
            .send(msg)
            .await
            .map_err(|_| anyhow::anyhow!("Agent channel closed"))?;
        Ok(())
    }

    /// Send a `Delegate` message and wait for the response (with timeout).
    pub async fn delegate(
        &self,
        from: &str,
        to: &str,
        payload: &str,
        timeout: std::time::Duration,
    ) -> Result<String> {
        let (response_tx, response_rx) = oneshot::channel();

        let msg = AgentMessage {
            id: Uuid::new_v4(),
            from: from.to_string(),
            to: to.to_string(),
            kind: MessageKind::Delegate,
            payload: payload.to_string(),
            response_tx: Some(response_tx),
        };

        self.send(msg).await?;

        tokio::time::timeout(timeout, response_rx)
            .await
            .map_err(|_| anyhow::anyhow!("Delegation to '{to}' timed out after {timeout:?}"))?
            .map_err(|_| anyhow::anyhow!("Agent '{to}' dropped response channel"))
    }

    /// Check if an agent is registered
    pub async fn is_registered(&self, name: &str) -> bool {
        self.senders.read().await.contains_key(name)
    }

    /// List registered agent names
    pub async fn registered_agents(&self) -> Vec<String> {
        self.senders.read().await.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn register_and_send() {
        let bus = AgentBus::new();
        let mut rx = bus.register("agent-a", 10).await;

        bus.send(AgentMessage {
            id: Uuid::new_v4(),
            from: "main".into(),
            to: "agent-a".into(),
            kind: MessageKind::Notify,
            payload: "hello".into(),
            response_tx: None,
        })
        .await
        .unwrap();

        let msg = rx.recv().await.unwrap();
        assert_eq!(msg.payload, "hello");
        assert_eq!(msg.kind, MessageKind::Notify);
        assert_eq!(msg.from, "main");
        assert_eq!(msg.to, "agent-a");
    }

    #[tokio::test]
    async fn delegate_with_response() {
        let bus = Arc::new(AgentBus::new());
        let mut rx = bus.register("worker", 10).await;

        let bus_clone = bus.clone();
        let handle = tokio::spawn(async move {
            bus_clone
                .delegate("main", "worker", "do stuff", Duration::from_secs(5))
                .await
        });

        // Simulate worker receiving and responding
        let msg = rx.recv().await.unwrap();
        assert_eq!(msg.kind, MessageKind::Delegate);
        assert_eq!(msg.payload, "do stuff");
        msg.response_tx.unwrap().send("done".into()).unwrap();

        let result = handle.await.unwrap().unwrap();
        assert_eq!(result, "done");
    }

    #[tokio::test]
    async fn delegate_timeout() {
        let bus = AgentBus::new();
        let _rx = bus.register("slow", 10).await;

        let result = bus
            .delegate("main", "slow", "task", Duration::from_millis(10))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn send_to_unregistered_fails() {
        let bus = AgentBus::new();
        let result = bus
            .send(AgentMessage {
                id: Uuid::new_v4(),
                from: "main".into(),
                to: "nobody".into(),
                kind: MessageKind::Notify,
                payload: "hello".into(),
                response_tx: None,
            })
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not registered"));
    }

    #[tokio::test]
    async fn unregister_removes_agent() {
        let bus = AgentBus::new();
        let _rx = bus.register("temp", 10).await;
        assert!(bus.is_registered("temp").await);
        bus.unregister("temp").await;
        assert!(!bus.is_registered("temp").await);
    }

    #[tokio::test]
    async fn registered_agents_list() {
        let bus = AgentBus::new();
        let _rx1 = bus.register("alpha", 10).await;
        let _rx2 = bus.register("beta", 10).await;

        let mut agents = bus.registered_agents().await;
        agents.sort();
        assert_eq!(agents, vec!["alpha", "beta"]);
    }

    #[tokio::test]
    async fn send_after_receiver_dropped_fails() {
        let bus = AgentBus::new();
        let rx = bus.register("dropper", 10).await;
        drop(rx);

        let result = bus
            .send(AgentMessage {
                id: Uuid::new_v4(),
                from: "main".into(),
                to: "dropper".into(),
                kind: MessageKind::Notify,
                payload: "hello".into(),
                response_tx: None,
            })
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn delegate_to_unregistered_fails() {
        let bus = AgentBus::new();
        let result = bus
            .delegate("main", "ghost", "task", Duration::from_secs(1))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not registered"));
    }

    #[tokio::test]
    async fn delegate_response_dropped() {
        let bus = Arc::new(AgentBus::new());
        let mut rx = bus.register("dropper", 10).await;

        let bus_clone = bus.clone();
        let handle = tokio::spawn(async move {
            bus_clone
                .delegate("main", "dropper", "task", Duration::from_secs(5))
                .await
        });

        // Receive message but drop the response_tx without sending
        let msg = rx.recv().await.unwrap();
        drop(msg.response_tx);

        let result = handle.await.unwrap();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("dropped"));
    }

    #[tokio::test]
    async fn multiple_messages_in_order() {
        let bus = AgentBus::new();
        let mut rx = bus.register("ordered", 10).await;

        for i in 0..5 {
            bus.send(AgentMessage {
                id: Uuid::new_v4(),
                from: "main".into(),
                to: "ordered".into(),
                kind: MessageKind::Notify,
                payload: format!("msg-{i}"),
                response_tx: None,
            })
            .await
            .unwrap();
        }

        for i in 0..5 {
            let msg = rx.recv().await.unwrap();
            assert_eq!(msg.payload, format!("msg-{i}"));
        }
    }

    #[tokio::test]
    async fn message_kinds() {
        let bus = AgentBus::new();
        let mut rx = bus.register("tester", 10).await;

        for kind in [
            MessageKind::Delegate,
            MessageKind::Result,
            MessageKind::Notify,
            MessageKind::Shutdown,
        ] {
            bus.send(AgentMessage {
                id: Uuid::new_v4(),
                from: "main".into(),
                to: "tester".into(),
                kind: kind.clone(),
                payload: String::new(),
                response_tx: None,
            })
            .await
            .unwrap();

            let msg = rx.recv().await.unwrap();
            assert_eq!(msg.kind, kind);
        }
    }
}
