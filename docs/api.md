# API

The API documents itself: every endpoint, its parameters and its answers, with
examples, are in the reference the server serves, generated from the code that
answers, so it cannot disagree with it.

| Address | |
|---|---|
| `/api/docs` | The reference, as a page to read and try calls from. |
| `/api/openapi.json` | The OpenAPI document behind it, for clients and code generators. |

Both are in the visitor's language, as the pages are: English or Spanish,
chosen with `?lang=en` or `?lang=es`, else from the language chosen on the site
(a cookie), else from the browser's.

This page is about what is not in the reference: the original addresses,
which keep working, and what they answer differently from the original scheme.

## Version 1

The documented API is under `/api/v1`: nodes, instances (and adding one),
their daily history, and the areas an instance can be in. Its addresses and
the shape of its answers do not change once published; anything that would
break a client goes into a next version, served beside it.

## The original addresses

Before `/api/v1` there was an original scheme, which other sites use. Its
addresses keep answering in its shapes:

| Address | Answers |
|---|---|
| `GET /api/nodes?days_active=N` | `{nodes, total_nodes}`: the nodes with a position, each with `source_url` and `source_name`, and how many nodes there are with or without one. |
| `GET /api/instances` | `{instances}`, each with `name`, `url`, `version`, `git_revision_short`, `node_count`, `last_updated_at` and `trend_30d`. |
| `POST /api/instances` | Adds an instance, as `POST /api/v1/instances` does. |
| `GET /api/histogram?days=N` | `{days, start_date, end_date, instances}`, each with `name`, `url` and its daily `points`. |
| `GET /out/node-link?source_url=…&node_id=…` | A redirect to the node's page on its instance. |

New clients should use `/api/v1`, which says more and is documented.

## Differences from the original scheme

| | Original scheme | Meshview Rust Planet |
|---|---|---|
| `name`, `source_name` | Free text, whatever the submitter typed | The area's English name, as "Galicia, Spain", from its codes. |
| `POST /api/instances` | `{name, url}`, with any name | `{url, country, region}`, the area from `/api/v1/areas`. A `name` without a `country` is refused with 400 and `area_required`. |
| Adding an instance | Accepted as typed | Accepted only if it answers as a Meshview there and then, at an address on the public Internet; a few attempts per hour per address. Published at once, with its nodes. |
| Instances that stop answering | Kept, with their last figures | Offline after three days without answering: last in the list and their nodes off the map. Deleted after thirty days offline. Both configurable. |
| `/api/nodes` | Every node of every instance | The nodes of the instances that answer, or answered in the latest days. |
| `/out/node-link` | A redirect to any `source_url` | Only to an instance on the list, and only to a node's page: it was an open redirect. 404 otherwise, 400 for a `node_id` that is not a node number. |
| CORS | None on `/api/instances` | Every `GET` may be read from any page. |
| Caching | None | Every `GET` carries an `ETag`; asked again with `If-None-Match`, an unchanged answer is a 304. Answers are gzipped for clients that accept it. |
| Errors | Plain text | JSON, `{"error": "...", "code": "..."}`. |
| Daily history | From its own collection | From the first day of collection here, and whatever an import brings. |
