# Blindspot documentation

The user guides follow repository code; a checkout may be ahead of the published download.
Use the `START-HERE.md` and `FEATURE-GUIDE.md` bundled with a release for that version.
[Makefile](../Makefile) owns the build version and supported macOS target.

## Use the app

- [Install and get started](quick-start.md): release download, native onboarding, permissions,
  first local model and document indexing.
- [Complete feature guide](new-features.md): shortcuts, workflows, limitations and release notes.
- [Search inside files](search-quickstart.md): folders, word/meaning search, passages and diagnostics.
- [Containers](new-features.md#local-containers): Docker/Podman, images, .env creation, monitoring and logs.
- [Time zones](time-quickstart.md): offline time lookup and conversion.
- [Media gallery](../media/README.md): native screenshots, recordings and reproducible demo capture.

## Build and maintain

- [Contributor setup](../SETUP.md): build, install and checks without editor-specific requirements.
- [Architecture](architecture.md): current components, data boundaries and native platform limits.
- [Release instructions](releasing.md): user-run GitHub publication with `./scripts/release.sh`.
- [Working agreement](../AGENTS.md), [source map](agent-map.md), and [agent workflow](agent-workflow.md).
- [Task notes](tasks/README.md): scoped decisions, validation and handoffs.
- [Third-party license sources](licenses/SOURCES.md).

## Historical reports

These preserve measurements and decisions for their recorded revisions. Pending items, installed
versions, test counts, model choices and machine snapshots are not current product status or
instructions to repair live state. Follow the guides above and verify source before acting.

- [Original architecture baseline](history/architecture-baseline-2026-09-13.md).
- [Foundational engineering report](engineering-report.md).
- [Roadmap checkpoints](roadmap-status.md).
- [Early content-index implementation and measurements](content-index.md).
- [Early semantic-search implementation and measurements](semantic-search.md).
- [Passage-search design](semantic-plan.md), [P2 handoff](semantic-p2-handoff.md), and
  [search-upgrade implementation report](search-implementation.md).
- [September 15 engineering handoff](history/engineering-handoff-2026-09-15.md) and
  [earlier Claude handoff](claude-pre-024-historical.md).
- Release snapshots: [0.2.0](release-0.2.0.md), [0.3.1](release-0.3.1.md).
