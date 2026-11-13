# Bundled browser libraries

Meshview World serves its third-party JavaScript, CSS, fonts and images from
this directory, built into the binary: no CDN is contacted by the pages, only
the map tile servers.

`manifest.json` records each package's version, npm archive URL, archive
SHA-256 checksum and the files taken from it. The files are unmodified
upstream ones, with their licenses beside them. Most come from Meshview's own
vendor directory, at the same versions, so both sites look and behave alike.

- Leaflet 1.9.4 and Leaflet.markercluster 1.5.3 for the map.
- ECharts 5.5.0 for the history chart.
- Bootstrap 5.3.3, its CSS only.
- Inter 5.3.0 (`@fontsource-variable/inter`, SIL Open Font License 1.1), the
  variable weight axis, normal and italic.
- Tabler Icons 3.51.0 (MIT), only the SVG files the pages use, mapped to
  `.ti-<name>` classes in `/static/icons.css`.

To update a library, download its versioned npm archive, check its checksum,
copy the files into a new versioned directory, update `manifest.json` and the
references in the templates, check the pages in a browser, and remove the old
directory. Versioned directories are served as immutable.
