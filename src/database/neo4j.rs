use neo4rs::{ConfigBuilder, Graph};

pub async fn create_graph(uri: &str, user: &str, password: &str) -> Result<Graph, neo4rs::Error> {
    let cfg = ConfigBuilder::default()
        .uri(uri)
        .user(user)
        .password(password)
        .build()?;
    Graph::connect(cfg).await
}
