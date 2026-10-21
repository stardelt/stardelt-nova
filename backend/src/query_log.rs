//! Who may see which row of the query log (decision D25, stardelt-nova#116).
//!
//! Pure functions, no I/O: the collector, the store and the HTTP API are separate tickets and
//! call [`visible`] and [`in_scope`] so the rule exists exactly once.

/// The fields of a log row that decide who may see it.
pub struct RowOwner<'a> {
    /// Engine user that ran the query (the `preferred_username` of a person or a service principal).
    pub principal: &'a str,
    /// Owner group when `principal` is a service principal; `None` when it is a person.
    pub owner_group: Option<&'a str>,
}

/// The signed-in person asking.
pub struct Viewer<'a> {
    /// The engine user Nova knows from sign-in (`preferred_username`).
    pub username: &'a str,
    /// The OIDC subject. Once authz 8/8 is live (with Superset impersonation,
    /// stardelt-platform#310) the engine user of a query is this opaque id; the query log
    /// records it as `principal`, so "mine" must match either.
    pub sub: &'a str,
    pub groups: &'a [String],
    pub is_admin: bool,
}

/// The `scope` query parameter of `GET /api/query-log`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Queries run by the viewer.
    Mine,
    /// Queries of service principals owned by a group the viewer belongs to.
    Team,
    /// Everything; administrators only.
    All,
}

/// Does `row` belong in `scope` for `viewer`? `Scope::All` is never granted to a non-administrator.
pub fn in_scope(scope: Scope, row: &RowOwner<'_>, viewer: &Viewer<'_>) -> bool {
    match scope {
        Scope::Mine => row.principal == viewer.username || row.principal == viewer.sub,
        Scope::Team => row
            .owner_group
            .is_some_and(|group| viewer.groups.iter().any(|g| g == group)),
        Scope::All => viewer.is_admin,
    }
}

/// May `viewer` see `row` at all? Own queries, queries of service principals the viewer's group
/// owns, and, for administrators, everything.
pub fn visible(row: &RowOwner<'_>, viewer: &Viewer<'_>) -> bool {
    [Scope::Mine, Scope::Team, Scope::All]
        .into_iter()
        .any(|scope| in_scope(scope, row, viewer))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    fn person(principal: &str) -> RowOwner<'_> {
        RowOwner {
            principal,
            owner_group: None,
        }
    }

    fn service_principal<'a>(principal: &'a str, group: &'a str) -> RowOwner<'a> {
        RowOwner {
            principal,
            owner_group: Some(group),
        }
    }

    #[test]
    fn a_person_sees_their_own_queries_only() {
        let g = groups(&["analysts"]);
        let alice = Viewer {
            username: "alice",
            sub: "alice",
            groups: &g,
            is_admin: false,
        };
        assert!(visible(&person("alice"), &alice));
        assert!(!visible(&person("bob"), &alice));
    }

    #[test]
    fn a_service_principal_is_visible_to_its_owner_group_only() {
        let owners = groups(&["data-eng"]);
        let others = groups(&["analysts"]);
        let row = service_principal("airflow-etl", "data-eng");
        let member = Viewer {
            username: "carol",
            sub: "carol",
            groups: &owners,
            is_admin: false,
        };
        let outsider = Viewer {
            username: "dave",
            sub: "dave",
            groups: &others,
            is_admin: false,
        };
        assert!(visible(&row, &member));
        assert!(!visible(&row, &outsider));
    }

    #[test]
    fn an_administrator_sees_everything() {
        let g = groups(&[]);
        let admin = Viewer {
            username: "root",
            sub: "root",
            groups: &g,
            is_admin: true,
        };
        assert!(visible(&person("bob"), &admin));
        assert!(visible(
            &service_principal("airflow-etl", "data-eng"),
            &admin
        ));
    }

    #[test]
    fn the_all_scope_is_for_administrators_only() {
        let g = groups(&["data-eng"]);
        let user = Viewer {
            username: "carol",
            sub: "carol",
            groups: &g,
            is_admin: false,
        };
        let row = person("carol");
        assert!(!in_scope(Scope::All, &row, &user));
        assert!(in_scope(Scope::Mine, &row, &user));
    }

    #[test]
    fn the_mine_scope_matches_the_subject_when_the_engine_user_is_a_sub() {
        let g = groups(&["analysts"]);
        let alice = Viewer {
            username: "alice",
            sub: "0b6b1f0e-4f0e-4d4e-8e3a-1f2a3b4c5d6e",
            groups: &g,
            is_admin: false,
        };
        // With authz 8/8 the engine user is the opaque subject, not the sign-in name.
        assert!(in_scope(Scope::Mine, &person(alice.sub), &alice));
        // The sign-in name still matches, for rows the engine ran before the switch.
        assert!(in_scope(Scope::Mine, &person("alice"), &alice));
        // Somebody else's query is not mine, under either name.
        assert!(!in_scope(Scope::Mine, &person("bob"), &alice));
        assert!(!in_scope(
            Scope::Mine,
            &person("1c7c2f1f-5a1f-4e5f-9f4b-2a3b4c5d6e7f"),
            &alice
        ));
    }

    #[test]
    fn the_team_scope_does_not_include_a_persons_queries() {
        let g = groups(&["data-eng"]);
        let user = Viewer {
            username: "carol",
            sub: "carol",
            groups: &g,
            is_admin: false,
        };
        assert!(!in_scope(Scope::Team, &person("carol"), &user));
        assert!(in_scope(
            Scope::Team,
            &service_principal("airflow-etl", "data-eng"),
            &user
        ));
    }
}
