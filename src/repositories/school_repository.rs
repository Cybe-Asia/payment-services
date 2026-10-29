use neo4rs::{Graph, Query};
use std::fmt;

#[derive(Debug)]
pub enum SchoolLookupError {
    Graph(neo4rs::Error),
    /// More than one School node answers to the same code. The caller must
    /// not guess; the duplicate has to be removed from the graph.
    Ambiguous {
        code: String,
        matches: usize,
    },
}

impl fmt::Display for SchoolLookupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Graph(e) => write!(f, "{e}"),
            Self::Ambiguous { code, matches } => write!(
                f,
                "school code {code} matches {matches} School nodes; refusing to pick one"
            ),
        }
    }
}

impl std::error::Error for SchoolLookupError {}

impl From<neo4rs::Error> for SchoolLookupError {
    fn from(e: neo4rs::Error) -> Self {
        Self::Graph(e)
    }
}

/// Resolves `IIHS` or `SCH-IIHS` to the School's `school_id`. Returns `None`
/// when no School matches and `Ambiguous` when more than one does.
pub async fn find_school_id_by_code(
    graph: &Graph,
    tenant_id: &str,
    school_code: &str,
) -> Result<Option<String>, SchoolLookupError> {
    let raw_code = school_code.trim().to_uppercase();
    let canonical_code = canonical_school_code(&raw_code);
    let school_id_alias = format!("SCH-{canonical_code}");

    let q = Query::new(
        "MATCH (s:School {tenant_id:$tenant_id}) \
         WHERE s.school_code = $raw_code \
            OR s.school_code = $canonical_code \
            OR s.school_code = $school_id_alias \
            OR s.school_id = $raw_code \
            OR s.school_id = $school_id_alias \
         RETURN s.school_id AS id LIMIT 3"
            .to_string(),
    )
    .param("tenant_id", tenant_id.to_string())
    .param("raw_code", raw_code)
    .param("canonical_code", canonical_code.clone())
    .param("school_id_alias", school_id_alias);
    let mut result = graph.execute(q).await?;
    let mut ids = Vec::new();
    while let Some(row) = result.next().await? {
        ids.push(row.get::<String>("id"));
    }
    pick_single_school(&canonical_code, ids)
}

/// One entry per matching School node (`None` when the node has no id).
fn pick_single_school(
    code: &str,
    matches: Vec<Option<String>>,
) -> Result<Option<String>, SchoolLookupError> {
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.into_iter().next().flatten()),
        n => Err(SchoolLookupError::Ambiguous {
            code: code.to_string(),
            matches: n,
        }),
    }
}

fn canonical_school_code(school_code: &str) -> String {
    school_code
        .strip_prefix("SCH-")
        .or_else(|| school_code.strip_prefix("SCHOOL-"))
        .unwrap_or(school_code)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{canonical_school_code, pick_single_school, SchoolLookupError};

    #[test]
    fn canonical_school_code_accepts_prefixed_and_bare_codes() {
        assert_eq!(canonical_school_code("SCH-IIHS"), "IIHS");
        assert_eq!(canonical_school_code("SCHOOL-IISS"), "IISS");
        assert_eq!(canonical_school_code("IIHS"), "IIHS");
    }

    #[test]
    fn one_match_resolves_to_its_school_id() {
        let found = pick_single_school("IIHS", vec![Some("SCH-IIHS".to_string())]).unwrap();
        assert_eq!(found.as_deref(), Some("SCH-IIHS"));
    }

    #[test]
    fn no_match_is_none() {
        assert!(pick_single_school("IIHS", vec![]).unwrap().is_none());
    }

    #[test]
    fn two_matches_are_refused_not_guessed() {
        let err = pick_single_school(
            "IIHS",
            vec![
                Some("SCH-IIHS".to_string()),
                Some("SCHOOL-IIHS".to_string()),
            ],
        )
        .unwrap_err();
        assert!(matches!(
            err,
            SchoolLookupError::Ambiguous { ref code, matches: 2 } if code == "IIHS"
        ));
        assert!(err.to_string().contains("refusing to pick one"));
    }
}
