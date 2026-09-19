CREATE TABLE catalog (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    installation TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision>=0)
);
-- JSON is the typed row authority. Generated columns cannot drift from it.
CREATE TABLE entities (
    id TEXT PRIMARY KEY,
    body TEXT NOT NULL CHECK(json_valid(body)),
    kind TEXT GENERATED ALWAYS AS (json_extract(body,'$.kind')) STORED,
    area_project TEXT GENERATED ALWAYS AS (CASE WHEN kind='work_area' THEN json_extract(body,'$.value.project') END) STORED REFERENCES entities(id),
    home_project TEXT GENERATED ALWAYS AS (CASE WHEN json_extract(body,'$.value.home.kind')='project' THEN json_extract(body,'$.value.home.id') END) STORED REFERENCES entities(id),
    home_area TEXT GENERATED ALWAYS AS (CASE WHEN json_extract(body,'$.value.home.kind')='work_area' THEN json_extract(body,'$.value.home.id') END) STORED REFERENCES entities(id),
    repository TEXT GENERATED ALWAYS AS (json_extract(body,'$.value.kind.repository')) STORED REFERENCES entities(id),
    CHECK(kind IN ('project','repository','work_area','location')),
    CHECK(id=json_extract(body,'$.value.id'))
);
CREATE INDEX entities_kind ON entities(kind,id);
CREATE INDEX entities_project ON entities(area_project,id);
CREATE INDEX entities_home_project ON entities(home_project,id);
CREATE INDEX entities_home_area ON entities(home_area,id);
CREATE INDEX entities_repository ON entities(repository,id);
CREATE TABLE associations (
    project TEXT NOT NULL REFERENCES entities(id), repository TEXT NOT NULL REFERENCES entities(id),
    PRIMARY KEY(project,repository)
);
CREATE TABLE bindings (
    location TEXT PRIMARY KEY REFERENCES entities(id), body TEXT NOT NULL CHECK(json_valid(body)),
    live_key TEXT UNIQUE
);
CREATE TABLE volume_defaults (volume TEXT PRIMARY KEY, body TEXT NOT NULL CHECK(json_valid(body)));
CREATE TABLE reviews (id TEXT PRIMARY KEY, kind TEXT NOT NULL, body TEXT NOT NULL CHECK(json_valid(body)));
CREATE TABLE receipts (request TEXT PRIMARY KEY, input_digest TEXT NOT NULL, body TEXT NOT NULL CHECK(json_valid(body)));
CREATE TABLE operations (
    id TEXT PRIMARY KEY, kind TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('pending','complete','failed','recovery_required')),
    body TEXT NOT NULL CHECK(json_valid(body))
);
CREATE TABLE operation_targets (operation TEXT NOT NULL REFERENCES operations(id), target TEXT NOT NULL REFERENCES entities(id), PRIMARY KEY(operation,target));
CREATE TABLE session_index (session TEXT PRIMARY KEY, target TEXT NOT NULL REFERENCES entities(id), body TEXT NOT NULL CHECK(json_valid(body)));
CREATE INDEX sessions_target ON session_index(target,session);
CREATE TABLE grants (id TEXT PRIMARY KEY, body TEXT NOT NULL CHECK(json_valid(body)));
CREATE TABLE proposals (id TEXT PRIMARY KEY, body TEXT NOT NULL CHECK(json_valid(body)));
CREATE TABLE grant_references (grant_id TEXT NOT NULL REFERENCES grants(id), target TEXT NOT NULL REFERENCES entities(id), PRIMARY KEY(grant_id,target));
CREATE TABLE proposal_references (proposal_id TEXT NOT NULL REFERENCES proposals(id), target TEXT NOT NULL REFERENCES entities(id), PRIMARY KEY(proposal_id,target));
CREATE TABLE closed_history (location TEXT PRIMARY KEY REFERENCES entities(id), body TEXT NOT NULL CHECK(json_valid(body)));
PRAGMA user_version=1;
