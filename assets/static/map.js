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
    const channelPanel = document.getElementById("filter-container");
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
    // (see planet.css), so both share the tiles the browser has.
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

    // The map's credit, in the footer while it shows (map.html).
    document.getElementById("footer-attribution").innerHTML =
        '&copy; <a href="https://www.openstreetmap.org/copyright" target="_blank" rel="noopener">OpenStreetMap</a> contributors';
    map.attributionControl.setPrefix(false);

    // The map's tools, under the layer picker: fit the nodes shown, full
    // screen, and copy a link to this view, with its filters.
    const ICONS = {
        fit: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 8v-2a2 2 0 0 1 2 -2h2M4 16v2a2 2 0 0 0 2 2h2M16 4h2a2 2 0 0 1 2 2v2M16 20h2a2 2 0 0 0 2 -2v-2M8 11a3 3 0 1 0 6 0a3 3 0 0 0 -6 0M16 16l-2.5 -2.5"/></svg>',
        link: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M9 15l6 -6M11 6l.463 -.536a5 5 0 0 1 7.071 7.072l-.534 .464M13 18l-.397 .534a5.068 5.068 0 0 1 -7.127 0a4.972 4.972 0 0 1 0 -7.071l.524 -.463"/></svg>',
        enter: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M16 4h4v4M14 10l6 -6M8 20h-4v-4M4 20l6 -6M16 20h4v-4M14 14l6 6M8 4h-4v4M4 4l6 6"/></svg>',
        exit: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M5 9h4v-4M3 3l6 6M5 15h4v4M3 21l6 -6M19 9h-4v-4M15 9l6 -6M19 15h-4v4M15 15l6 6"/></svg>',
        copied: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M5 12l5 5l10 -10"/></svg>',
    };
    const Tools = L.Control.extend({
        options: {position: "topright"},
        onAdd() {
            const bar = L.DomUtil.create("div", "leaflet-bar planet-map-tools");
            const button = (icon, label, action) => {
                const element = L.DomUtil.create("button", "", bar);
                element.type = "button";
                element.title = label;
                element.setAttribute("aria-label", label);
                element.innerHTML = ICONS[icon];
                L.DomEvent.on(element, "click", () => action(element));
                return element;
            };
            button("fit", MeshI18n.t("Fit the nodes shown"), () => fitShown());
            kioskButton = button("enter", MeshI18n.t("Full screen"), () => toggleKiosk());
            kioskButton.id = "kiosk-button";
            button("link", MeshI18n.t("Copy a link to this view"), async element => {
                saveState();
                try {
                    await navigator.clipboard.writeText(location.href);
                    element.classList.add("is-copied");
                    element.innerHTML = ICONS.copied;
                    setTimeout(() => {
                        element.classList.remove("is-copied");
                        element.innerHTML = ICONS.link;
                    }, 1600);
                } catch {
                    window.prompt(MeshI18n.t("Copy a link to this view"), location.href);
                }
            });
            bar.lastElementChild.id = "share-button";
            L.DomEvent.disableClickPropagation(bar);
            return bar;
        },
    });
    let kioskButton = null;
    new Tools().addTo(map);

    // Full screen: the page turns into its kiosk mode where it is, the browser
    // fills the screen with it, and leaving the browser's full screen (Esc)
    // leaves both. Loaded as a kiosk (?kiosk), only the browser's full screen
    // comes and goes. Without that (iPhones), kiosk mode alone. As Meshview.
    const root = document.documentElement;
    const loadedAsKiosk = root.classList.contains("mesh-kiosk");
    const kioskOn = () => loadedAsKiosk ? Boolean(document.fullscreenElement) : root.classList.contains("mesh-kiosk");
    function syncKiosk() {
        const on = kioskOn();
        const label = on ? MeshI18n.t("Exit full screen") : MeshI18n.t("Full screen");
        kioskButton.title = label;
        kioskButton.setAttribute("aria-label", label);
        kioskButton.setAttribute("aria-pressed", String(on));
        kioskButton.innerHTML = on ? ICONS.exit : ICONS.enter;
        setTimeout(() => map.invalidateSize(), 50);
    }
    function setKiosk(on) {
        if (!loadedAsKiosk) root.classList.toggle("mesh-kiosk", on);
        syncKiosk();
    }
    function toggleKiosk() {
        if (!kioskOn()) {
            setKiosk(true);
            root.requestFullscreen?.().catch(() => {});
        } else {
            if (document.fullscreenElement) document.exitFullscreen().catch(() => {});
            setKiosk(false);
        }
    }
    document.addEventListener("fullscreenchange", () => {
        if (!document.fullscreenElement) setKiosk(false); else syncKiosk();
    });
    syncKiosk();

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

    // A node's popup, laid out as Meshview's: its short name on top, linked
    // to its page on its instance, its long name under it, its id, and its
    // facts, leaving out those it has none for.
    function popup(node) {
        const instance = instances.get(node.instance_url);
        const id = node.node_id;
        const hex = `!${Number(id).toString(16).padStart(8, "0")}`;
        const unnamed = MeshI18n.t("Node {id}", {id});
        const short = node.short_name || "";
        const long = node.long_name && node.long_name !== short ? node.long_name : "";
        const page = `${node.instance_url}/node/${encodeURIComponent(id)}`;
        const area = instance ? instance.area[MeshI18n.language] || instance.area.en : hostOf(node.instance_url);
        const facts = [
            [MeshI18n.t("Instance"), `<a href="${escapeHtml(node.instance_url)}/map" target="_blank" rel="noopener">${escapeHtml(area)}</a>`, true],
            [MeshI18n.t("Channel"), node.channel],
            [MeshI18n.t("Hardware"), node.hw_model],
            [MeshI18n.t("Firmware"), node.firmware],
            [MeshI18n.t("MQTT gateway"), node.is_mqtt_gateway ? MeshI18n.t("Yes") : null],
            [MeshI18n.t("First seen"), node.first_seen_us ? formatMoment(node.first_seen_us / 1e6) : null],
            [MeshI18n.t("Last seen"), node.last_seen_us ? formatMoment(node.last_seen_us / 1e6) : null],
        ].filter(([, value]) => value)
            .map(([label, value, html]) => `<dt>${escapeHtml(label)}</dt><dd>${html ? value : escapeHtml(value)}</dd>`)
            .join("");
        const meta = [node.role, `${unnamed} (${hex})`].filter(Boolean).join(" · ");
        return `<div class="node-popup">
            <a class="node-popup-short" href="${escapeHtml(page)}" target="_blank" rel="noopener">${escapeHtml(short || long || unnamed)}</a>
            ${short && long ? `<div class="node-popup-name">${escapeHtml(long)}</div>` : ""}
            <p class="node-popup-meta">${escapeHtml(meta)}</p>
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
        document.getElementById("summary-mapped").textContent = formatNumber(mapped);
        renderChannels(scoped);
        shownMarkers = shown;
        if (fit) fitShown();
        saveState();
    }

    // The nodes shown, all in view; none, the world.
    let shownMarkers = [];
    function fitShown() {
        if (shownMarkers.length) {
            map.fitBounds(L.latLngBounds(shownMarkers.map(marker => marker.getLatLng())), {padding: [30, 30], maxZoom: 11});
        } else {
            map.setView([25, 0], 2);
        }
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
        channelPanel.hidden = groups.length === 0;
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
        url.searchParams.delete("full");
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
        status.classList.remove("is-error");
        status.classList.add("is-loading");
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
            status.classList.replace("is-loading", "is-error");
            status.textContent = MeshI18n.t("The nodes could not be loaded. Try again in a while.");
        }
    }

    fillDays();
    // On a phone the figures and the channels start folded, and what is done
    // there is not kept: unfolded, they cover much of so small a map.
    const phone = window.matchMedia("(max-width:700px)").matches;
    if (phone) document.getElementById("planet-summary").open = false;
    (() => {
        const KEY = "meshview-planet-channels-folded";
        const toggle = document.getElementById("map-legend-toggle");
        const set = folded => {
            channelPanel.classList.toggle("is-folded", folded);
            toggle.setAttribute("aria-expanded", String(!folded));
            const label = folded ? MeshI18n.t("Show the channels") : MeshI18n.t("Hide the channels");
            toggle.title = label;
            toggle.setAttribute("aria-label", label);
        };
        let folded = phone;
        if (!phone) {
            try { folded = localStorage.getItem(KEY) === "1"; } catch {}
        }
        set(folded);
        toggle.addEventListener("click", () => {
            folded = !folded;
            set(folded);
            if (phone) return;
            try { folded ? localStorage.setItem(KEY, "1") : localStorage.removeItem(KEY); } catch {}
        });
    })();
    daysSelect.addEventListener("change", () => load());
    countrySelect.addEventListener("change", () => {
        fillInstances();
        apply({fit: true});
    });
    instanceSelect.addEventListener("change", () => apply({fit: true}));
    load({fit: Boolean(params.get("country") || params.get("instance"))});
})();
