use memzoi_core::{
    MemoryDestination, MemoryDestinationPolicy, MemoryPlane, MemoryReviewRequirement,
    MemoryWriteRoute, RepoMemoryExclusion, TWO_PLANE_MEMORY_POLICY,
};

const REPO_POLICY: MemoryDestinationPolicy = MemoryDestination::Repo.policy();
const LOCAL_POLICY: MemoryDestinationPolicy = MemoryDestination::Local.policy();
const SESSION_POLICY: MemoryDestinationPolicy = MemoryDestination::Session.policy();
const DISCARD_POLICY: MemoryDestinationPolicy = MemoryDestination::Discard.policy();
const NEEDS_REVIEW_POLICY: MemoryDestinationPolicy = MemoryDestination::NeedsReview.policy();

#[test]
fn destinations_are_ordered_and_have_stable_public_wire_values() {
    let expected = [
        (MemoryDestination::Repo, "repo"),
        (MemoryDestination::Local, "local"),
        (MemoryDestination::Session, "session"),
        (MemoryDestination::Discard, "discard"),
        (MemoryDestination::NeedsReview, "needs_review"),
    ];

    let actual = MemoryDestination::ALL.map(|destination| (destination, destination.as_str()));
    assert_eq!(actual, expected);

    for (destination, serialized) in expected {
        assert_eq!(destination.as_str(), serialized);
        assert_eq!(destination.to_string(), serialized);
        assert_eq!(
            serde_json::to_string(&destination).unwrap(),
            format!("\"{serialized}\"")
        );
        assert_eq!(
            serde_json::from_str::<MemoryDestination>(&format!("\"{serialized}\"")).unwrap(),
            destination
        );
    }
}

#[test]
fn every_destination_route_review_and_plane_is_explicit() {
    let expected = [
        (
            MemoryDestination::Repo,
            Some(MemoryPlane::Git),
            MemoryWriteRoute::FileBackedProposal,
            MemoryReviewRequirement::ProposalReview,
        ),
        (
            MemoryDestination::Local,
            Some(MemoryPlane::Runtime),
            MemoryWriteRoute::RuntimeLocal,
            MemoryReviewRequirement::NoReview,
        ),
        (
            MemoryDestination::Session,
            Some(MemoryPlane::Runtime),
            MemoryWriteRoute::RuntimeSession,
            MemoryReviewRequirement::NoReview,
        ),
        (
            MemoryDestination::Discard,
            None,
            MemoryWriteRoute::NoWrite,
            MemoryReviewRequirement::NoReview,
        ),
        (
            MemoryDestination::NeedsReview,
            None,
            MemoryWriteRoute::NoWrite,
            MemoryReviewRequirement::HumanDecision,
        ),
    ];

    for (policy, (destination, plane, write_route, review)) in [
        REPO_POLICY,
        LOCAL_POLICY,
        SESSION_POLICY,
        DISCARD_POLICY,
        NEEDS_REVIEW_POLICY,
    ]
    .into_iter()
    .zip(expected)
    {
        assert_eq!(policy, destination.policy());
        assert_eq!(
            policy.destination, destination,
            "policy destination mismatch for {destination}"
        );
        assert_eq!(policy.plane, plane, "unexpected plane for {destination}");
        assert_eq!(
            policy.write_route, write_route,
            "unexpected write route for {destination}"
        );
        assert_eq!(
            policy.review, review,
            "unexpected review requirement for {destination}"
        );
    }
}

#[test]
fn enum_surfaces_use_stable_snake_case_values() {
    macro_rules! assert_surface {
        ($value:expr, $expected:literal) => {{
            let value = $value;
            assert_eq!(value.as_str(), $expected);
            assert_eq!(value.to_string(), $expected);
            assert_eq!(
                serde_json::to_string(&value).unwrap(),
                format!("\"{}\"", $expected)
            );
        }};
    }

    assert_surface!(MemoryPlane::Git, "git");
    assert_surface!(MemoryPlane::Runtime, "runtime");

    assert_surface!(MemoryWriteRoute::FileBackedProposal, "file_backed_proposal");
    assert_surface!(MemoryWriteRoute::RuntimeLocal, "runtime_local");
    assert_surface!(MemoryWriteRoute::RuntimeSession, "runtime_session");
    assert_surface!(MemoryWriteRoute::NoWrite, "no_write");

    assert_surface!(MemoryReviewRequirement::ProposalReview, "proposal_review");
    assert_surface!(MemoryReviewRequirement::NoReview, "no_review");
    assert_surface!(MemoryReviewRequirement::HumanDecision, "human_decision");

    assert_surface!(RepoMemoryExclusion::Secrets, "secrets");
    assert_surface!(
        RepoMemoryExclusion::RawChatTranscripts,
        "raw_chat_transcripts"
    );
    assert_surface!(
        RepoMemoryExclusion::PrivatePersonalData,
        "private_personal_data"
    );
    assert_surface!(
        RepoMemoryExclusion::TemporaryTaskState,
        "temporary_task_state"
    );
    assert_surface!(RepoMemoryExclusion::LocalOnlyState, "local_only_state");
}

#[test]
fn policy_metadata_is_static_and_ordered() {
    assert_eq!(
        TWO_PLANE_MEMORY_POLICY.canonical_records_glob,
        ".memzoi/records/*.md"
    );
    assert_eq!(
        TWO_PLANE_MEMORY_POLICY.runtime_project_root_template,
        "${MEMZOI_HOME:-~/.memzoi}/projects/<project-key>/"
    );

    assert_eq!(
        TWO_PLANE_MEMORY_POLICY.repo_exclusions,
        &[
            RepoMemoryExclusion::Secrets,
            RepoMemoryExclusion::RawChatTranscripts,
            RepoMemoryExclusion::PrivatePersonalData,
            RepoMemoryExclusion::TemporaryTaskState,
            RepoMemoryExclusion::LocalOnlyState,
        ]
    );

    let exclusions = TWO_PLANE_MEMORY_POLICY
        .repo_exclusions
        .iter()
        .map(|exclusion| exclusion.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        exclusions,
        [
            "secrets",
            "raw_chat_transcripts",
            "private_personal_data",
            "temporary_task_state",
            "local_only_state",
        ]
    );
    assert_eq!(
        TWO_PLANE_MEMORY_POLICY.future_destinations,
        &["team", "cloud"]
    );
}

#[test]
fn future_destinations_are_not_current_write_destinations() {
    for label in TWO_PLANE_MEMORY_POLICY.future_destinations {
        assert!(
            serde_json::from_str::<MemoryDestination>(&format!("\"{label}\"")).is_err(),
            "future destination {label:?} must not be accepted as an MVP destination"
        );
    }
}

#[test]
fn no_plane_destinations_are_no_write_outcomes() {
    for destination in [MemoryDestination::Discard, MemoryDestination::NeedsReview] {
        let policy = destination.policy();
        assert_eq!(policy.plane, None);
        assert_eq!(policy.write_route, MemoryWriteRoute::NoWrite);
    }
    assert_eq!(
        MemoryDestination::Discard.policy().review,
        MemoryReviewRequirement::NoReview
    );
    assert_eq!(
        MemoryDestination::NeedsReview.policy().review,
        MemoryReviewRequirement::HumanDecision
    );
}
