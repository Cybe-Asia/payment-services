use neo4rs::{query, ConfigBuilder, Graph};

#[derive(Debug)]
pub struct GraphCheckFailure {
    pub stage: &'static str,
    pub category: &'static str,
    pub io_kind: Option<std::io::ErrorKind>,
}

fn safe_failure(stage: &'static str, error: neo4rs::Error) -> GraphCheckFailure {
    use neo4rs::Error;
    let category = match &error {
        Error::IOError { .. } => "io",
        Error::AuthenticationError(_) => "auth",
        Error::UrlParseError(_) | Error::UnsupportedScheme(_) | Error::InvalidDnsName(_) => "uri",
        Error::ConnectionError => "pool",
        // neo4rs 0.6 wraps server FAILURE responses in UnexpectedMessage.
        Error::UnexpectedMessage(message) if message.contains("Failure(") => "query",
        Error::UnsupportedVersion(_) | Error::UnexpectedMessage(_) | Error::UnknownMessage(_) => {
            "protocol"
        }
        Error::UnknownType(_)
        | Error::InvalidTypeMarker(_)
        | Error::DeserializationError(_)
        | Error::ConversionError => "deserialize",
        Error::InvalidConfig => "config",
        Error::StringTooLong | Error::MapTooBig | Error::BytesTooBig | Error::ListTooLong => {
            "query"
        }
    };
    let io_kind = match &error {
        Error::IOError { detail } => Some(detail.kind()),
        _ => None,
    };
    GraphCheckFailure {
        stage,
        category,
        io_kind,
    }
}

fn configured_value(name: &str) -> Result<String, GraphCheckFailure> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or(GraphCheckFailure {
            stage: "config",
            category: "config",
            io_kind: None,
        })
}

/// This diagnostic requires explicit graph settings and does not load other Payment configuration.
pub async fn check_configured_graph_connection() -> Result<(), GraphCheckFailure> {
    let uri = configured_value("NEO4J_URI")?;
    let user = configured_value("NEO4J_USER")?;
    let password = configured_value("NEO4J_PASSWORD")?;
    check_graph_connection(&uri, &user, &password).await
}

/// Exercise both run/DISCARD and execute/PULL without schema or data writes.
pub async fn check_graph_connection(
    uri: &str,
    user: &str,
    password: &str,
) -> Result<(), GraphCheckFailure> {
    let check = async {
        let graph = create_graph(uri, user, password)
            .await
            .map_err(|error| safe_failure("connect", error))?;
        graph
            .run(query("RETURN 1 AS ok"))
            .await
            .map_err(|error| safe_failure("run", error))?;
        let mut rows = graph
            .execute(query("RETURN 1 AS ok"))
            .await
            .map_err(|error| safe_failure("execute", error))?;
        let row = rows
            .next()
            .await
            .map_err(|error| safe_failure("pull", error))?;
        let valid = row.as_ref().and_then(|row| row.get::<i64>("ok")) == Some(1);
        let extra = rows
            .next()
            .await
            .map_err(|error| safe_failure("pull", error))?;
        if !valid || extra.is_some() {
            return Err(GraphCheckFailure {
                stage: "response",
                category: "query",
                io_kind: None,
            });
        }
        Ok(())
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), check)
        .await
        .unwrap_or(Err(GraphCheckFailure {
            stage: "check",
            category: "io",
            io_kind: Some(std::io::ErrorKind::TimedOut),
        }))
}

pub async fn create_graph(uri: &str, user: &str, password: &str) -> Result<Graph, neo4rs::Error> {
    let cfg = ConfigBuilder::default()
        .uri(uri)
        .user(user)
        .password(password)
        .build()?;
    Graph::connect(cfg).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn absent_graph_setting_returns_safe_config_error() {
        let error =
            configured_value("__PAYMENT_GRAPH_DIAGNOSTIC_SYNTHETIC_UNSET_SETTING__").unwrap_err();
        assert_eq!(error.stage, "config");
        assert_eq!(error.category, "config");
        assert_eq!(error.io_kind, None);
    }

    #[test]
    fn diagnostic_categories_never_retain_raw_error_text() {
        let sensitive = "synthetic-secret-url-query-row-must-not-be-logged";
        use neo4rs::Error;
        for (error, expected) in [
            (Error::AuthenticationError(sensitive.into()), "auth"),
            (Error::UnsupportedScheme(sensitive.into()), "uri"),
            (Error::InvalidDnsName(sensitive.into()), "uri"),
            (Error::ConnectionError, "pool"),
            (Error::UnsupportedVersion(sensitive.into()), "protocol"),
            (Error::UnexpectedMessage(sensitive.into()), "protocol"),
            (
                Error::UnexpectedMessage(format!("Failure({sensitive})")),
                "query",
            ),
            (Error::DeserializationError(sensitive.into()), "deserialize"),
            (Error::InvalidTypeMarker(sensitive.into()), "deserialize"),
            (Error::UnknownType(sensitive.into()), "deserialize"),
            (Error::InvalidConfig, "config"),
            (Error::StringTooLong, "query"),
        ] {
            let safe = safe_failure("run", error);
            assert_eq!(safe.category, expected);
            assert!(!format!("{safe:?}").contains(sensitive));
        }
        let safe = safe_failure(
            "run",
            Error::IOError {
                detail: std::io::Error::new(std::io::ErrorKind::ConnectionRefused, sensitive),
            },
        );
        assert_eq!(safe.category, "io");
        assert_eq!(safe.io_kind, Some(std::io::ErrorKind::ConnectionRefused));
        assert!(!format!("{safe:?}").contains(sensitive));
    }

    async fn frame(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
        let mut bytes = Vec::new();
        loop {
            let size = stream.read_u16().await.unwrap();
            if size == 0 {
                return bytes;
            }
            let offset = bytes.len();
            bytes.resize(offset + size as usize, 0);
            stream.read_exact(&mut bytes[offset..]).await.unwrap();
        }
    }

    async fn send(stream: &mut tokio::net::TcpStream, bytes: &[u8]) {
        stream.write_u16(bytes.len() as u16).await.unwrap();
        stream.write_all(bytes).await.unwrap();
        stream.write_u16(0).await.unwrap();
        stream.flush().await.unwrap();
    }

    async fn fixture(
        value: u8,
        duplicate: bool,
        reject_run: bool,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let uri = format!("bolt://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut handshake = [0; 20];
            stream.read_exact(&mut handshake).await.unwrap();
            assert_eq!(&handshake[..4], &[0x60, 0x60, 0xb0, 0x17]);
            stream.write_all(&[0, 0, 1, 4]).await.unwrap();
            let hello = frame(&mut stream).await;
            assert_eq!(hello[1], 1);
            send(&mut stream, &[0xb1, 0x70, 0xa0]).await;
            let mut runs = 0;
            loop {
                let request = frame(&mut stream).await;
                match request[1] {
                    0x0f => send(&mut stream, &[0xb1, 0x70, 0xa0]).await,
                    0x10 => {
                        let query = b"RETURN 1 AS ok";
                        assert!(request.windows(query.len()).any(|bytes| bytes == query));
                        runs += 1;
                        if reject_run {
                            send(
                                &mut stream,
                                &[
                                    0xb1, 0x7f, 0xa1, 0x87, b'm', b'e', b's', b's', b'a', b'g',
                                    b'e', 0x88, b'r', b'e', b'j', b'e', b'c', b't', b'e', b'd',
                                ],
                            )
                            .await;
                            return;
                        }
                        send(
                            &mut stream,
                            &[
                                0xb1, 0x70, 0xa1, 0x86, b'f', b'i', b'e', b'l', b'd', b's', 0x91,
                                0x82, b'o', b'k',
                            ],
                        )
                        .await;
                    }
                    0x2f => {
                        assert_eq!(runs, 1);
                        send(&mut stream, &[0xb1, 0x70, 0xa0]).await;
                    }
                    0x3f => {
                        assert_eq!(runs, 2);
                        send(&mut stream, &[0xb1, 0x71, 0x91, value]).await;
                        if duplicate {
                            send(&mut stream, &[0xb1, 0x71, 0x91, value]).await;
                        }
                        send(&mut stream, &[0xb1, 0x70, 0xa0]).await;
                        return;
                    }
                    _ => panic!("unexpected diagnostic Bolt operation"),
                }
            }
        });
        (uri, task)
    }

    #[tokio::test]
    async fn read_only_check_requires_run_and_exactly_one_verified_result() {
        for (value, duplicate, reject_run) in [
            (1, false, false),
            (2, false, false),
            (1, true, false),
            (1, false, true),
        ] {
            let (uri, task) = fixture(value, duplicate, reject_run).await;
            let result = check_graph_connection(&uri, "synthetic-user", "synthetic-password").await;
            task.await.unwrap();
            if value == 1 && !duplicate && !reject_run {
                assert!(result.is_ok());
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.stage, if reject_run { "run" } else { "response" });
                assert_eq!(error.category, "query");
            }
        }
    }

    #[tokio::test]
    async fn lazy_connect_failure_reports_safe_io_kind_at_run_boundary() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let uri = format!("bolt://{}", listener.local_addr().unwrap());
        drop(listener);
        let error = check_graph_connection(&uri, "synthetic-user", "synthetic-password")
            .await
            .unwrap_err();
        assert_eq!(error.stage, "run");
        assert_eq!(error.category, "io");
        assert_eq!(error.io_kind, Some(std::io::ErrorKind::ConnectionRefused));
    }
}
