use serde::Serialize;

pub const MODULE_IDS: [&str; 13] = [
    "chatgpt",
    "grok",
    "tiktok",
    "zai",
    "doordash",
    "uber",
    "sephora",
    "stockx",
    "airbnb",
    "spotify",
    "kick",
    "instagram",
    "reddit",
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub category: &'static str,
    pub description: &'static str,
    pub enabled: bool,
}

pub fn is_known_module(id: &str) -> bool {
    MODULE_IDS.contains(&id)
}

pub fn is_enabled_module(id: &str) -> bool {
    matches!(id, "chatgpt")
}

pub fn modules() -> Vec<ModuleInfo> {
    vec![
        module(
            "chatgpt",
            "ChatGPT",
            "AI",
            "AI assistant for writing, research, coding, and everyday work",
        ),
        module(
            "grok",
            "Grok",
            "AI",
            "AI assistant from xAI for information, creativity, and problem-solving",
        ),
        module(
            "tiktok",
            "TikTok",
            "Social",
            "Short-form video platform for discovering, creating, and sharing content",
        ),
        module(
            "zai",
            "zAI",
            "AI",
            "AI platform for GLM models, assistants, and developer tools",
        ),
        module(
            "doordash",
            "DoorDash",
            "Marketplace",
            "Local delivery platform connecting customers with restaurants and stores",
        ),
        module(
            "uber",
            "Uber",
            "Marketplace",
            "Mobility and delivery platform for rides, food, and local services",
        ),
        module(
            "sephora",
            "Sephora",
            "Marketplace",
            "Beauty retailer for cosmetics, skincare, fragrance, and personal care",
        ),
        module(
            "stockx",
            "StockX",
            "Marketplace",
            "Marketplace for sneakers, streetwear, collectibles, and verified goods",
        ),
        module(
            "airbnb",
            "Airbnb",
            "Marketplace",
            "Travel platform for booking homes, stays, and local experiences",
        ),
        module(
            "spotify",
            "Spotify",
            "Entertainment",
            "Audio streaming platform for music, podcasts, and audiobooks",
        ),
        module(
            "kick",
            "Kick",
            "Social",
            "Live-streaming platform for creators, communities, and entertainment",
        ),
        module(
            "instagram",
            "Instagram",
            "Social",
            "Social platform for sharing photos, videos, stories, and messages",
        ),
        module(
            "reddit",
            "Reddit",
            "Social",
            "Community platform for discussions, news, and interest-based conversations",
        ),
    ]
}

fn module(
    id: &'static str,
    name: &'static str,
    category: &'static str,
    description: &'static str,
) -> ModuleInfo {
    ModuleInfo {
        id,
        name,
        category,
        description,
        enabled: is_enabled_module(id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ids_are_unique_and_known() {
        let modules = modules();
        assert_eq!(modules.len(), MODULE_IDS.len());

        for (index, item) in modules.iter().enumerate() {
            assert!(is_known_module(item.id));
            assert!(!modules[..index].iter().any(|other| other.id == item.id));
        }

        let enabled: Vec<_> = modules.iter().filter(|item| item.enabled).collect();
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].id, "chatgpt");
        assert!(is_enabled_module("chatgpt"));
        assert!(!is_enabled_module("reddit"));
    }
}
