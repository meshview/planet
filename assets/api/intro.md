The nodes every listed Meshview instance hears, gathered on one map of the world, and the instances themselves: what they run, how many nodes they see, and how that changes day by day.

## How the data comes in

Every few hours the server asks each instance for its `/version` and for the nodes it heard in the latest days, from its `/api/v1/nodes` or, on older versions, its `/api/nodes`. What an instance answers replaces what it had answered before. An instance that does not answer keeps its data and its error is recorded; after some days it goes offline, which takes its nodes off the map, and after some more it is deleted. Nothing here is live: the data is as fresh as the latest collection.

## Conventions

- Everything is answered as JSON, errors too: `{"error": "...", "code": "..."}`, where `error` is for a person and `code` for a program. 400 is a parameter or a body that is not valid, 404 something that is not there, and 500 a failure on the server's side.
- Reading needs no token or key, and any web page may read: every `GET` answers with `Access-Control-Allow-Origin: *`. Adding an instance is for this site's own pages, and each address may try a few times an hour.
- Times ending in `_us` are microseconds since the Unix epoch, as Meshview gives them; times ending in `_at` and `_since` are seconds. Dates are UTC days, `YYYY-MM-DD`.
- Positions are in degrees, `lat` and `lon`, and null for a node with none. Nodes on the map are spread by a few metres in the site's pages, never in the API.
- A node is known by its number, `node_id`, the Meshtastic node number; its `id` is the same number as Meshtastic writes it, `!` and eight hex digits. A node heard by two instances is there once for each.
- An instance is known by its `url`, in the one form it is stored in: lower case scheme and host, no trailing slash.
- An instance's area is a country, ISO 3166-1 alpha-2 (`ES`), and optionally one of its top level regions, ISO 3166-2 (`ES-GA`), from `/api/v1/areas`. Its names come in English and Spanish under `area`.
- Names, roles, channels and everything else about a node are text from the meshes, written by whoever set the node up: never HTML to trust.

## Caching

The data only changes when an instance is collected or added, so every answer carries an `ETag` and `Cache-Control: no-cache`. Ask again with `If-None-Match` and the tag, and an answer that has not changed is a 304 with no body. Answers are gzipped for clients that accept it: the full list of nodes is several megabytes of JSON, and about a sixth of that compressed.

## Versions

This is version 1, under `/api/v1`. Its addresses and the shape of its answers do not change once published; what would break a client goes into a next version, served beside it. `/version` and `/livez` are outside any version.

The addresses of Meshview Planet, which this takes over from, keep answering in its shapes: `/api/nodes`, `/api/instances`, `/api/histogram` and `/out/node-link`. They are not described here; new clients should use `/api/v1`.

## Examples

The nodes with a position heard in the latest three days, those of the instances in Spain, every instance with its counts and its status, and the nodes per day of every instance in the latest three months:

```sh
curl --compressed 'https://planet.example.org/api/v1/nodes?days_active=3&has_position=true'
curl --compressed 'https://planet.example.org/api/v1/nodes?country=ES'
curl 'https://planet.example.org/api/v1/instances'
curl 'https://planet.example.org/api/v1/histogram?days=90'
```

Adding an instance in Galicia, Spain:

```sh
curl -X POST -H 'Content-Type: application/json' \
  -d '{"url": "https://meshview.example.org", "country": "ES", "region": "ES-GA"}' \
  'https://planet.example.org/api/v1/instances'
```
