/* The world map: every node with a position, from every instance that
   answers, clustered, coloured by channel as Meshview colours them, and
   filtered by how recently it was heard, country, instance and channel.

   Node names and everything else from the meshes are escaped before they
   reach the page: they are whatever their owners typed. */
(() => {
    "use strict";

    // Meshview's colours for the channels the official presets name, compared
    // without case, spaces, hyphens or underscores. Any other channel is grey.
    const PRESET_COLOURS = {
        longslow: "#133a6c", longfast: "#245ea8", longmod: "#617dd1", longturbo: "#1f96d6",
        mediumslow: "#0b5a5f", mediumfast: "#12858b", mediumturbo: "#1aa39c",
        shortslow: "#9c4506", shortfast: "#e07012", shortturbo: "#f29a3f",
        narrowslow: "#4b2a8a", narrowfast: "#7a4cc8",
        liteslow: "#8a1d51", litefast: "#cf3d7e",
        tinyslow: "#765800", tinyfast: "#b38600",
    };
    const OTHER = "other";
    const OTHER_COLOUR = "#6e6e6e";
    const DAY_CHOICES = [1, 3, 7, 14, 30];

    const presetOf = channel => {
        const key = String(channel || "").toLowerCase().replace(/[\s_-]+/g, "");
        return Object.hasOwn(PRESET_COLOURS, key) ? key : OTHER;
    };
    const colourOf = group => PRESET_COLOURS[group] || OTHER_COLOUR;

    // A small, steady offset per node, so that nodes at one spot (two
    // instances hearing the same node, or a node and its neighbour on one
    // roof) can be told apart. As Meshview does.
    function hashToUnit(text) {
        let h = 2166136261;
        for (let i = 0; i < text.length; i++) {
            h ^= text.charCodeAt(i);
            h = Math.imul(h, 16777619);
        }
        return (h >>> 0) / 0xffffffff;
    }
    function jitter(lat, lon, key) {
        const metres = 15;
        const angle = hashToUnit(key) * Math.PI * 2;
        const r = metres * (0.3 + 0.7 * hashToUnit(`r:${key}`));
        const dLat = (r * Math.cos(angle)) / 111320;
        const dLon = (r * Math.sin(angle)) / (111320 * Math.cos(lat * Math.PI / 180));
        return [lat + dLat, lon + dLon];
    }

    const regionNames = (() => {
        try { return new Intl.DisplayNames([MeshI18n.language], {type: "region"}); }
        catch { return null; }
    })();
    const countryName = code => regionNames?.of(code) || code;
    const hostOf = url => { try { return new URL(url).host + new URL(url).pathname.replace(/\/$/, ""); } catch { return url; } };

    // The state of the page.
    const params = new URLSearchParams(location.search);
    const daysSelect = document.getElementById("filter-days");
    const countrySelect = document.getElementById("filter-country");
    const instanceSelect = document.getElementById("filter-instance");
    const channelBox = document.getElementById("channel-filters");
    const status = document.getElementById("map-status");
    const hiddenChannels = new Set();
    let instances = new Map();
    let nodes = [];
    let markers = [];

    // The map.
    const map = L.map("map", {preferCanvas: true, zoomSnap: 0.5, worldCopyJump: true}).setView([25, 0], 2);

    // Two looks for the map, both OpenStreetMap's tiles: as they are, and in
    // grey, without the greens and blues of the land and the sea, against
    // which the nodes' colours stand out. The grey is a filter on the tiles
    // (see theme-styles.html), so both share the tiles the browser has.
    const STYLE_KEY = "meshview-planet-map-style";
    const tiles = () => L.tileLayer("https://tile.openstreetmap.org/{z}/{x}/{y}.png", {
        maxZoom: 19,
        attribution: '&copy; <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a> contributors',
    });
    const standard = tiles();
    const grey = tiles();
    let style = "standard";
    try { if (localStorage.getItem(STYLE_KEY) === "grey") style = "grey"; } catch {}
    (style === "grey" ? grey : standard).addTo(map);
    map.getContainer().classList.toggle("is-grey", style === "grey");
    const layers = L.control.layers({
        [MeshI18n.t("Standard map")]: standard,
        [MeshI18n.t("Grey map")]: grey,
    }, null, {position: "topright"}).addTo(map);
    // Opened by a click (or Enter) only, not by passing the mouse over it;
    // a click on the map, or choosing a style, closes it.
    L.DomEvent.off(layers.getContainer(), {
        mouseenter: layers._expandSafely,
        mouseleave: layers.collapse,
    }, layers);
    map.on("baselayerchange", event => {
        layers.collapse();
        const isGrey = event.layer === grey;
        map.getContainer().classList.toggle("is-grey", isGrey);
        try { localStorage.setItem(STYLE_KEY, isGrey ? "grey" : "standard"); } catch {}
    });

    // Full screen: only the header, the map and the channels.
    const FullScreen = L.Control.extend({
        options: {position: "topleft"},
        onAdd() {
            const bar = L.DomUtil.create("div", "leaflet-bar");
            const button = L.DomUtil.create("button", "planet-map-button", bar);
            button.type = "button";
            const icon = L.DomUtil.create("span", "ti", button);
            icon.setAttribute("aria-hidden", "true");
            L.DomEvent.disableClickPropagation(bar);
            L.DomEvent.on(button, "click", () => setFull(!document.body.classList.contains("is-map-full"), {remember: true}));
            this.update = full => {
                const label = full ? MeshI18n.t("Leave full screen") : MeshI18n.t("Full screen map");
                button.title = label;
                button.setAttribute("aria-label", label);
                button.setAttribute("aria-pressed", String(full));
                icon.className = `ti ${full ? "ti-arrows-minimize" : "ti-arrows-maximize"}`;
            };
            this.update(false);
            return bar;
        },
    });
    const fullScreen = new FullScreen().addTo(map);
    const header = document.querySelector(".mesh-header");
    // Whether the visitor left the map full screen, for their next visit. Only
    // what they chose themselves: opening a link with ?full=1 changes nothing.
    const FULL_KEY = "meshview-planet-map-full";
    function setFull(full, {remember = false} = {}) {
        if (remember) {
            try { localStorage.setItem(FULL_KEY, full ? "1" : "0"); } catch {}
        }
        document.body.classList.toggle("is-map-full", full);
        document.documentElement.style.setProperty("--header-height", `${header.offsetHeight}px`);
        header.classList.remove("is-hidden");
        fullScreen.update(full);
        map.invalidateSize();
        saveState();
    }
    document.addEventListener("keydown", event => {
        if (event.key === "Escape" && document.body.classList.contains("is-map-full")) setFull(false, {remember: true});
    });
    window.addEventListener("resize", () => {
        document.documentElement.style.setProperty("--header-height", `${header.offsetHeight}px`);
    });
    const clusters = L.markerClusterGroup({
        chunkedLoading: true,
        maxClusterRadius: 45,
        disableClusteringAtZoom: 11,
        spiderfyOnMaxZoom: false,
        showCoverageOnHover: false,
        iconCreateFunction(cluster) {
            const count = cluster.getChildCount();
            const size = count < 10 ? 30 : count < 100 ? 36 : count < 1000 ? 44 : 52;
            return L.divIcon({
                html: `<span>${escapeHtml(formatNumber(count))}</span>`,
                className: "planet-cluster",
                iconSize: L.point(size, size),
            });
        },
    });
    map.addLayer(clusters);
    // For the browser's console, and for tests that drive the page.
    window.meshPlanet = {map, clusters};

    function popup(node) {
        const instance = instances.get(node.instance_url);
        const id = node.node_id;
        const hex = `!${Number(id).toString(16).padStart(8, "0")}`;
        const name = node.long_name || node.short_name || MeshI18n.t("Node {id}", {id});
        const short = node.short_name && node.short_name !== name
            ? ` <span class="node-popup-short">(${escapeHtml(node.short_name)})</span>` : "";
        const page = `${node.instance_url}/node/${encodeURIComponent(id)}`;
        const area = instance ? instance.area[MeshI18n.language] || instance.area.en : hostOf(node.instance_url);
        const facts = [
            [MeshI18n.t("Instance"), `<a href="${escapeHtml(node.instance_url)}/map" target="_blank" rel="noopener">${escapeHtml(area)}</a>`, true],
            [MeshI18n.t("Role"), node.role],
            [MeshI18n.t("Channel"), node.channel],
            [MeshI18n.t("Hardware"), node.hw_model],
            [MeshI18n.t("Firmware"), node.firmware],
            [MeshI18n.t("MQTT gateway"), node.is_mqtt_gateway ? MeshI18n.t("Yes") : null],
            [MeshI18n.t("First seen"), node.first_seen_us ? formatMoment(node.first_seen_us / 1e6) : null],
            [MeshI18n.t("Last seen"), node.last_seen_us ? formatMoment(node.last_seen_us / 1e6) : null],
        ].filter(([, value]) => value)
            .map(([label, value, html]) => `<dt>${escapeHtml(label)}</dt><dd>${html ? value : escapeHtml(value)}</dd>`)
            .join("");
        return `<div class="node-popup">
            <div class="node-popup-title"><a href="${escapeHtml(page)}" target="_blank" rel="noopener">${escapeHtml(name)}</a>${short}</div>
            <p class="node-popup-meta">${escapeHtml(MeshI18n.t("Node {id}", {id}))} (${hex})</p>
            <dl class="node-popup-facts">${facts}</dl>
        </div>`;
    }

    function selectedFilters() {
        return {
            country: countrySelect.value,
            instance: instanceSelect.value,
        };
    }

    // Whether a node passes the country and instance filters; the channels
    // are counted after these, so that their counts say what each would add.
    function inScope(node, {country, instance}) {
        if (instance && node.instance_url !== instance) return false;
        if (country && instances.get(node.instance_url)?.country !== country) return false;
        return true;
    }

    function apply({fit = false} = {}) {
        const filters = selectedFilters();
        const scoped = nodes.filter(node => inScope(node, filters));
        const shown = [];
        const sources = new Set();
        let mapped = 0;
        let heard = 0;
        for (const node of scoped) {
            if (hiddenChannels.has(presetOf(node.channel))) continue;
            heard++;
            sources.add(node.instance_url);
            if (node.marker) {
                mapped++;
                shown.push(node.marker);
            }
        }
        clusters.clearLayers();
        clusters.addLayers(shown);
        document.getElementById("metric-instances").textContent = formatNumber(sources.size);
        document.getElementById("metric-countries").textContent = formatNumber(
            new Set([...sources].map(url => instances.get(url)?.country).filter(Boolean)).size);
        document.getElementById("metric-nodes").textContent = formatNumber(heard);
        document.getElementById("metric-mapped").textContent = formatNumber(mapped);
        document.getElementById("header-nodes").textContent = formatNumber(heard);
        document.getElementById("header-mapped").textContent = formatNumber(mapped);
        renderChannels(scoped);
        if (fit && shown.length) {
            map.fitBounds(L.latLngBounds(shown.map(marker => marker.getLatLng())), {padding: [30, 30], maxZoom: 11});
        } else if (fit) {
            map.setView([25, 0], 2);
        }
        saveState();
    }

    function renderChannels(scoped) {
        const counts = new Map();
        const names = new Map();
        for (const node of scoped) {
            if (!node.marker) continue;
            const group = presetOf(node.channel);
            counts.set(group, (counts.get(group) || 0) + 1);
            if (group !== OTHER && !names.has(group)) names.set(group, node.channel);
        }
        const groups = [...counts.keys()].sort((a, b) =>
            a === OTHER ? 1 : b === OTHER ? -1 : counts.get(b) - counts.get(a));
        channelBox.replaceChildren(...groups.map(group => {
            const label = document.createElement("label");
            label.className = "map-filter";
            label.style.setProperty("--chip-color", colourOf(group));
            const input = document.createElement("input");
            input.type = "checkbox";
            input.checked = !hiddenChannels.has(group);
            input.addEventListener("change", () => {
                if (input.checked) hiddenChannels.delete(group); else hiddenChannels.add(group);
                apply();
            });
            const dot = document.createElement("span");
            dot.className = "map-filter-dot";
            const text = document.createElement("span");
            text.textContent = group === OTHER ? MeshI18n.t("Other channels") : names.get(group);
            const count = document.createElement("small");
            count.textContent = formatNumber(counts.get(group));
            label.append(input, dot, text, count);
            return label;
        }));
    }

    function fillSelects() {
        const online = [...instances.values()].filter(instance => instance.status === "online");
        const keep = (select, value) => {
            if ([...select.options].some(option => option.value === value)) select.value = value;
        };
        const countries = [...new Set(online.map(instance => instance.country))]
            .map(code => [code, countryName(code)])
            .sort((a, b) => a[1].localeCompare(b[1], MeshI18n.language));
        countrySelect.replaceChildren(countrySelect.options[0], ...countries.map(([code, name]) => new Option(name, code)));
        keep(countrySelect, params.get("country") || "");
        fillInstances();
        keep(instanceSelect, params.get("instance") || "");
    }

    function fillInstances() {
        const country = countrySelect.value;
        const current = instanceSelect.value;
        const choices = [...instances.values()]
            .filter(instance => instance.status === "online" && (!country || instance.country === country))
            .map(instance => [instance.url, `${instance.area[MeshI18n.language] || instance.area.en} · ${hostOf(instance.url)}`])
            .sort((a, b) => a[1].localeCompare(b[1], MeshI18n.language));
        instanceSelect.replaceChildren(instanceSelect.options[0], ...choices.map(([url, name]) => new Option(name, url)));
        if (choices.some(([url]) => url === current)) instanceSelect.value = current;
    }

    function saveState() {
        const url = new URL(location.href);
        const set = (key, value, fallback) => {
            if (value && value !== fallback) url.searchParams.set(key, value); else url.searchParams.delete(key);
        };
        set("days", daysSelect.value, daysSelect.dataset.default);
        set("country", countrySelect.value, "");
        set("instance", instanceSelect.value, "");
        set("full", document.body.classList.contains("is-map-full") ? "1" : "", "");
        url.searchParams.delete("lang");
        history.replaceState(null, "", url.pathname + url.search + url.hash);
    }

    function fillDays() {
        const fallback = Number(daysSelect.dataset.default) || 14;
        const asked = Number(params.get("days"));
        const choices = [...new Set([...DAY_CHOICES, fallback])].sort((a, b) => a - b);
        daysSelect.replaceChildren(...choices.map(days => new Option(
            days === 1 ? MeshI18n.t("1 day") : MeshI18n.t("{days} days", {days}), String(days))));
        daysSelect.value = String(choices.includes(asked) ? asked : fallback);
    }

    async function getJson(url) {
        const response = await fetch(url);
        if (!response.ok) throw new Error(`${url}: ${response.status}`);
        return response.json();
    }

    async function load({fit = false} = {}) {
        status.hidden = false;
        status.className = "is-loading";
        status.textContent = MeshI18n.t("Loading nodes…");
        try {
            const [listed, answer] = await Promise.all([
                getJson("/api/v1/instances"),
                getJson(`/api/v1/nodes?days_active=${encodeURIComponent(daysSelect.value)}`),
            ]);
            instances = new Map(listed.instances.map(instance => [instance.url, instance]));
            nodes = answer.nodes;
            for (const node of nodes) {
                if (node.lat == null || node.lon == null) continue;
                const colour = colourOf(presetOf(node.channel));
                const position = jitter(node.lat, node.lon, `${node.instance_url}:${node.node_id}`);
                node.marker = L.circleMarker(position, {
                    radius: 6, weight: 1.5, color: "#ffffff", fillColor: colour, fillOpacity: 0.95,
                }).bindPopup(() => popup(node), {closeButton: false, maxWidth: 340});
            }
            fillSelects();
            status.hidden = true;
            apply({fit});
        } catch (error) {
            console.error(error);
            status.className = "is-error";
            status.textContent = MeshI18n.t("The nodes could not be loaded. Try again in a while.");
        }
    }

    fillDays();
    // The address says, if it does (?full=1 or ?full=0); else what the visitor
    // chose last time.
    let startFull = false;
    if (params.has("full")) {
        startFull = params.get("full") === "1";
    } else {
        try { startFull = localStorage.getItem(FULL_KEY) === "1"; } catch {}
    }
    if (startFull) setFull(true);
    daysSelect.addEventListener("change", () => load());
    countrySelect.addEventListener("change", () => {
        fillInstances();
        apply({fit: true});
    });
    instanceSelect.addEventListener("change", () => apply({fit: true}));
    load({fit: Boolean(params.get("country") || params.get("instance"))});
})();
