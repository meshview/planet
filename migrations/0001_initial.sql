-- The instances, the nodes each one sees, and how many it saw each day.

CREATE TABLE instance (
    id INTEGER PRIMARY KEY,
    -- Normalised: scheme and host in lower case, no trailing slash, no query.
    url TEXT NOT NULL UNIQUE,
    -- ISO 3166-1 alpha-2, and optionally a top level ISO 3166-2 subdivision of
    -- it. Never free text: the names shown are looked up from these.
    country TEXT NOT NULL,
    region TEXT,
    -- From its /version, as of the latest collection that read it.
    version TEXT,
    git_revision TEXT,
    git_revision_short TEXT,
    -- Unix seconds.
    added_at INTEGER NOT NULL,
    last_attempt_at INTEGER,
    last_success_at INTEGER,
    -- Why the latest collection failed; null once one succeeds.
    last_error TEXT,
    -- Set when it has not answered for long enough; null while it answers.
    offline_since INTEGER
);

CREATE TABLE node (
    instance_id INTEGER NOT NULL REFERENCES instance (id) ON DELETE CASCADE,
    node_id INTEGER NOT NULL,
    long_name TEXT,
    short_name TEXT,
    hw_model TEXT,
    firmware TEXT,
    role TEXT,
    channel TEXT,
    -- Degrees; null when the instance knows no position for it.
    lat REAL,
    lon REAL,
    is_mqtt_gateway INTEGER,
    first_seen_us INTEGER,
    last_seen_us INTEGER,
    PRIMARY KEY (instance_id, node_id)
);

CREATE INDEX node_last_seen ON node (last_seen_us);

CREATE TABLE instance_daily (
    instance_id INTEGER NOT NULL REFERENCES instance (id) ON DELETE CASCADE,
    -- YYYY-MM-DD, in UTC.
    date TEXT NOT NULL,
    -- The nodes of the latest good collection of that day.
    node_count INTEGER NOT NULL,
    -- Unix seconds.
    recorded_at INTEGER NOT NULL,
    PRIMARY KEY (instance_id, date)
);
