use std::collections::BTreeSet;

use memzoi_core::RepositoryWriteRoute;

#[test]
fn every_route_has_a_unique_stable_identifier() {
    assert_eq!(RepositoryWriteRoute::ALL.len(), 14);
    let identifiers = RepositoryWriteRoute::ALL
        .iter()
        .map(|route| route.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(identifiers.len(), RepositoryWriteRoute::ALL.len());
    assert!(identifiers.contains("materialization"));
    assert!(identifiers.contains("recovery"));
}
