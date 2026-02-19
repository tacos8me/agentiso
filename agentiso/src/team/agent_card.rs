use serde::{Deserialize, Serialize};

/// Status of an agent within a team.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Initializing,
    Ready,
    Working,
    Done,
    Failed,
}

/// Network endpoints for reaching an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEndpoints {
    pub vsock_cid: u32,
    pub ip: String,
    pub http_port: u16,
}

/// A2A-style agent card describing a team member.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCard {
    pub name: String,
    pub role: String,
    pub description: String,
    pub skills: Vec<String>,
    pub endpoints: AgentEndpoints,
    pub status: AgentStatus,
    pub team: String,
    pub workspace_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_status_serde_roundtrip() {
        let statuses = vec![
            AgentStatus::Initializing,
            AgentStatus::Ready,
            AgentStatus::Working,
            AgentStatus::Done,
            AgentStatus::Failed,
        ];
        for status in statuses {
            let json = serde_json::to_string(&status).unwrap();
            let deserialized: AgentStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, deserialized);
        }
    }

    #[test]
    fn agent_status_lowercase_serialization() {
        assert_eq!(serde_json::to_string(&AgentStatus::Initializing).unwrap(), "\"initializing\"");
        assert_eq!(serde_json::to_string(&AgentStatus::Ready).unwrap(), "\"ready\"");
        assert_eq!(serde_json::to_string(&AgentStatus::Working).unwrap(), "\"working\"");
        assert_eq!(serde_json::to_string(&AgentStatus::Done).unwrap(), "\"done\"");
        assert_eq!(serde_json::to_string(&AgentStatus::Failed).unwrap(), "\"failed\"");
    }

    #[test]
    fn agent_endpoints_serde_roundtrip() {
        let endpoints = AgentEndpoints {
            vsock_cid: 100,
            ip: "10.99.0.2".to_string(),
            http_port: 8080,
        };
        let json = serde_json::to_string(&endpoints).unwrap();
        let deserialized: AgentEndpoints = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.vsock_cid, 100);
        assert_eq!(deserialized.ip, "10.99.0.2");
        assert_eq!(deserialized.http_port, 8080);
    }

    #[test]
    fn agent_card_serde_roundtrip() {
        let card = AgentCard {
            name: "researcher".to_string(),
            role: "research".to_string(),
            description: "Researches topics".to_string(),
            skills: vec!["web_search".to_string(), "summarize".to_string()],
            endpoints: AgentEndpoints {
                vsock_cid: 100,
                ip: "10.99.0.2".to_string(),
                http_port: 8080,
            },
            status: AgentStatus::Ready,
            team: "my-team".to_string(),
            workspace_id: "abcd1234-0000-0000-0000-000000000000".to_string(),
        };

        let json = serde_json::to_string_pretty(&card).unwrap();
        let deserialized: AgentCard = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.name, "researcher");
        assert_eq!(deserialized.role, "research");
        assert_eq!(deserialized.skills.len(), 2);
        assert_eq!(deserialized.status, AgentStatus::Ready);
        assert_eq!(deserialized.team, "my-team");
        assert_eq!(deserialized.workspace_id, "abcd1234-0000-0000-0000-000000000000");
    }

    #[test]
    fn agent_card_deserialize_from_json_string() {
        let json = r#"{
            "name": "coder",
            "role": "implementation",
            "description": "Writes code",
            "skills": ["rust", "python"],
            "endpoints": {
                "vsock_cid": 101,
                "ip": "10.99.0.3",
                "http_port": 8080
            },
            "status": "working",
            "team": "dev-team",
            "workspace_id": "11111111-2222-3333-4444-555555555555"
        }"#;

        let card: AgentCard = serde_json::from_str(json).unwrap();
        assert_eq!(card.name, "coder");
        assert_eq!(card.status, AgentStatus::Working);
        assert_eq!(card.endpoints.vsock_cid, 101);
    }
}
