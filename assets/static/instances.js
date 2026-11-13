/* The instances page: figures, the daily history, the list, and the form to
   add one. Everything an instance answered is escaped before it reaches the
   page. */
(() => {
    "use strict";

    const language = MeshI18n.language;
    const areaOf = instance => instance.area[language] || instance.area.en;
    const hostOf = url => { try { const u = new URL(url); return u.host + u.pathname.replace(/\/$/, ""); } catch { return url; } };
    // Collections are hours apart: one that failed since the latest good one is
    // worth a word, before the instance is offline.
    const failing = instance => instance.status === "online" && instance.last_error;

    async function getJson(url) {
        const response = await fetch(url);
        if (!response.ok) throw new Error(`${url}: ${response.status}`);
        return response.json();
    }

    // --- The list ---

    function trend(instance) {
        const {start_count: start, end_count: end} = instance.trend_30d;
        if (!start) return `<span class="trend-flat">—</span>`;
        const change = (end - start) / start * 100;
        const rounded = Math.round(change * 10) / 10;
        const text = `${rounded > 0 ? "+" : ""}${new Intl.NumberFormat(language, {maximumFractionDigits: 1}).format(rounded)} %`;
        const kind = rounded > 0 ? "trend-up" : rounded < 0 ? "trend-down" : "trend-flat";
        return `<span class="${kind}" title="${escapeHtml(MeshI18n.t("{from} to {to} nodes", {from: formatNumber(start), to: formatNumber(end)}))}">${escapeHtml(text)}</span>`;
    }

    function statusCell(instance) {
        if (instance.status === "offline") {
            return `<span class="status-tag is-offline">${MeshI18n.h("Offline")}</span>`
                + `<span class="status-reason">${escapeHtml(MeshI18n.t("Since {when}", {when: formatAgo(instance.offline_since)}))}`
                + (instance.last_error ? ` · ${escapeHtml(instance.last_error)}` : "") + `</span>`;
        }
        if (failing(instance)) {
            return `<span class="status-tag is-failing">${MeshI18n.h("Not answering")}</span>`
                + `<span class="status-reason">${escapeHtml(instance.last_error)}</span>`;
        }
        return `<span class="status-tag">${MeshI18n.h("Online")}</span>`;
    }

    function row(instance) {
        const version = instance.version
            ? `${escapeHtml(instance.version)}${instance.git_revision_short && instance.git_revision_short !== "unknown"
                ? ` <small>(${escapeHtml(instance.git_revision_short)})</small>` : ""}`
            : "—";
        const collected = instance.last_success_at
            ? `<span title="${escapeHtml(formatMoment(instance.last_success_at))}">${escapeHtml(formatAgo(instance.last_success_at))}</span>`
            : MeshI18n.h("Never");
        return `<tr class="${instance.status === "offline" ? "is-offline" : ""}">
            <td class="c-area">${escapeHtml(areaOf(instance))}</td>
            <td class="c-url"><a href="${escapeHtml(instance.url)}" target="_blank" rel="noopener">${escapeHtml(hostOf(instance.url))}</a></td>
            <td class="c-version">${version}</td>
            <td class="is-number" data-label="${MeshI18n.h("Nodes")}">${escapeHtml(formatNumber(instance.node_count))}</td>
            <td class="is-number" data-label="${MeshI18n.h("On the map")}">${escapeHtml(formatNumber(instance.mapped_count))}</td>
            <td class="is-number" data-label="${MeshI18n.h("30 days")}">${trend(instance)}</td>
            <td data-label="${MeshI18n.h("Last collected")}">${collected}</td>
            <td class="c-status">${statusCell(instance)}</td>
        </tr>`;
    }

    // Sorting by a column. Whatever the column, the instances that answer come
    // first, then those that did not answer the latest time, then those that
    // are offline; without a column, the list is in the server's order.
    const collator = new Intl.Collator(language, {numeric: true, sensitivity: "base"});
    const trendOf = instance => {
        const {start_count: start, end_count: end} = instance.trend_30d;
        return start ? (end - start) / start : null;
    };
    const SORTS = {
        area: {text: true, key: areaOf},
        url: {text: true, key: instance => hostOf(instance.url)},
        // Numbered versions before named ones ("plus 1.0.0"), either way.
        version: {text: true, key: instance => instance.version, first: version => /^\d/.test(version) ? 0 : 1},
        nodes: {key: instance => instance.node_count},
        mapped: {key: instance => instance.mapped_count},
        trend: {key: trendOf},
        collected: {key: instance => instance.last_success_at},
        // Within each group, by area.
        status: {text: true, key: areaOf},
    };
    let listed = [];
    let sort = {column: null, descending: false};

    function compare(a, b) {
        const tier = instance => instance.status === "offline" ? 2 : failing(instance) ? 1 : 0;
        const group = tier(a) - tier(b);
        if (group || !sort.column) return group;
        const {text, key, first} = SORTS[sort.column];
        const [x, y] = [key(a), key(b)];
        // Nothing to sort by goes last, whichever the direction.
        if (x == null || x === "") return y == null || y === "" ? 0 : 1;
        if (y == null || y === "") return -1;
        const block = first ? first(x) - first(y) : 0;
        if (block) return block;
        const order = text ? collator.compare(String(x), String(y)) : x - y;
        return sort.descending ? -order : order;
    }

    function renderList() {
        const body = document.getElementById("instance-rows");
        const ordered = listed.map((instance, index) => ({instance, index}))
            .sort((a, b) => compare(a.instance, b.instance) || a.index - b.index)
            .map(({instance}) => instance);
        body.innerHTML = ordered.length
            ? ordered.map(row).join("")
            : `<tr><td colspan="8" class="planet-empty">${MeshI18n.h("No instances yet.")}</td></tr>`;
        document.querySelectorAll(".planet-table th[data-sort]").forEach(th => {
            const state = th.dataset.sort !== sort.column ? "none" : sort.descending ? "descending" : "ascending";
            th.setAttribute("aria-sort", state);
        });
    }

    document.querySelectorAll(".planet-table th[data-sort]").forEach(th => {
        th.querySelector("button").addEventListener("click", () => {
            const column = th.dataset.sort;
            if (sort.column === column) {
                sort.descending = !sort.descending;
            } else {
                // Names from A, figures and times from the largest.
                sort = {column, descending: !SORTS[column].text};
            }
            renderList();
        });
    });

    async function loadList() {
        const body = document.getElementById("instance-rows");
        try {
            const {instances} = await getJson("/api/v1/instances");
            listed = instances;
            const online = instances.filter(instance => instance.status === "online");
            const answering = online.filter(instance => !failing(instance));
            document.getElementById("metric-online").textContent = formatNumber(answering.length);
            document.getElementById("metric-offline").textContent = formatNumber(instances.length - online.length);
            document.getElementById("metric-nodes").textContent =
                formatNumber(online.reduce((sum, instance) => sum + instance.node_count, 0));
            document.getElementById("metric-countries").textContent =
                formatNumber(new Set(online.map(instance => instance.country)).size);
            document.getElementById("list-count").textContent = formatNumber(instances.length);
            renderList();
        } catch (error) {
            console.error(error);
            body.innerHTML = `<tr><td colspan="8" class="planet-empty">${MeshI18n.h("The instances could not be loaded. Try again in a while.")}</td></tr>`;
        }
    }

    // --- The history ---

    const chartElement = document.getElementById("history-chart");
    const chart = initMeshChart(chartElement);
    window.addEventListener("resize", () => chart.resize());

    // Every day from the first to the last, so that a day nobody answered
    // shows as a gap rather than being skipped.
    function daysBetween(start, end) {
        const days = [];
        for (let day = new Date(`${start}T00:00:00Z`); day <= new Date(`${end}T00:00:00Z`); day.setUTCDate(day.getUTCDate() + 1)) {
            days.push(day.toISOString().slice(0, 10));
        }
        return days;
    }

    async function loadHistory() {
        const days = document.getElementById("history-days").value;
        chart.showLoading({text: MeshI18n.t("Loading…"), color: "#245ea8", textColor: "#566579", maskColor: "rgba(0,0,0,0)"});
        try {
            const history = await getJson(`/api/v1/histogram?days=${encodeURIComponent(days)}`);
            const dates = daysBetween(history.start_date, history.end_date);
            const totals = new Map(dates.map(date => [date, null]));
            const series = history.instances.map(instance => {
                const counts = new Map(instance.points.map(point => [point.date, point.node_count]));
                for (const [date, count] of counts) totals.set(date, (totals.get(date) || 0) + count);
                return {
                    name: areaOf(instance) + " · " + hostOf(instance.url),
                    type: "line",
                    // A few days of history are points rather than lines.
                    showSymbol: dates.length <= 45,
                    symbolSize: 5,
                    connectNulls: false,
                    emphasis: {focus: "series"},
                    data: dates.map(date => counts.has(date) ? counts.get(date) : null),
                };
            });
            const total = MeshI18n.t("All instances");
            series.unshift({
                name: total, type: "line", showSymbol: dates.length <= 45, symbolSize: 6, yAxisIndex: 1,
                lineStyle: {width: 3, type: "dashed", color: "#243247"}, itemStyle: {color: "#243247"},
                data: dates.map(date => totals.get(date)),
            });
            chart.hideLoading();
            chart.setOption({
                animation: false,
                grid: {left: 56, right: 64, top: 24, bottom: 86},
                tooltip: {
                    trigger: "axis",
                    order: "valueDesc",
                    confine: true,
                    valueFormatter: value => value == null ? "—" : formatNumber(value),
                },
                legend: {type: "scroll", bottom: 0, textStyle: {color: "#566579"}},
                xAxis: {type: "category", data: dates, boundaryGap: false, axisLabel: {color: "#566579"}},
                yAxis: [
                    {type: "value", name: MeshI18n.t("Per instance"), axisLabel: {color: "#566579"}, nameTextStyle: {color: "#566579"}},
                    {type: "value", name: total, splitLine: {show: false}, axisLabel: {color: "#566579"}, nameTextStyle: {color: "#566579"}},
                ],
                series,
            }, true);
        } catch (error) {
            console.error(error);
            chart.hideLoading();
            chart.clear();
            chartElement.textContent = MeshI18n.t("The history could not be loaded. Try again in a while.");
        }
    }
    document.getElementById("history-days").addEventListener("change", loadHistory);

    // --- The form ---

    const form = document.getElementById("submit-form");
    if (form) {
        const country = document.getElementById("submit-country");
        const region = document.getElementById("submit-region");
        const button = document.getElementById("submit-button");
        const status = document.getElementById("submit-status");
        let areas = new Map();

        getJson(`/api/v1/areas?lang=${encodeURIComponent(language)}`).then(list => {
            areas = new Map(list.countries.map(choice => [choice.code, choice]));
            country.append(...list.countries.map(choice => new Option(choice.name, choice.code)));
        }).catch(error => console.error(error));

        country.addEventListener("change", () => {
            const regions = areas.get(country.value)?.regions || [];
            region.replaceChildren(region.options[0], ...regions.map(choice => new Option(choice.name, choice.code)));
            region.value = "";
            region.disabled = regions.length === 0;
        });

        const MESSAGES = {
            invalid_url: () => MeshI18n.t("That is not a valid http or https address."),
            private_address: () => MeshI18n.t("That address is on a private network, which this site cannot reach."),
            area_required: () => MeshI18n.t("Choose the country the instance is in."),
            unknown_country: () => MeshI18n.t("Choose the country the instance is in."),
            unknown_region: () => MeshI18n.t("Choose a region from the list, or the whole country."),
            duplicate: () => MeshI18n.t("That instance is already on the list."),
            blocked: () => MeshI18n.t("That instance is not accepted on this site."),
            not_meshview: () => MeshI18n.t("The instance did not answer as a Meshview does. Check the address, and that it can be reached from the Internet."),
            rate_limited: () => MeshI18n.t("Too many attempts from your address. Try again in an hour."),
            disabled: () => MeshI18n.t("New instances are not being accepted right now."),
        };

        function say(kind, text, detail) {
            status.className = kind;
            status.innerHTML = escapeHtml(text) + (detail ? `<small>${escapeHtml(detail)}</small>` : "");
        }

        form.addEventListener("submit", async event => {
            event.preventDefault();
            const url = document.getElementById("submit-url").value.trim();
            if (!url) return say("is-error", MeshI18n.t("Write the address of your Meshview."));
            if (!country.value) return say("is-error", MeshI18n.t("Choose the country the instance is in."));
            button.disabled = true;
            say("is-loading", MeshI18n.t("Asking the instance…"));
            try {
                const response = await fetch("/api/v1/instances", {
                    method: "POST",
                    headers: {"Content-Type": "application/json"},
                    body: JSON.stringify({url, country: country.value, region: region.value || null}),
                });
                const answer = await response.json().catch(() => ({}));
                if (response.ok) {
                    say("is-done", MeshI18n.t("Added {area}, with {nodes} nodes. They are on the map already.", {
                        area: answer.area?.[language] || answer.area?.en || "",
                        nodes: formatNumber(answer.node_count || 0),
                    }));
                    form.reset();
                    region.disabled = true;
                    loadList();
                    loadHistory();
                } else {
                    const message = MESSAGES[answer.code]?.() || MeshI18n.t("The instance could not be added. Try again in a while.");
                    say("is-error", message, answer.code === "not_meshview" ? answer.error : null);
                }
            } catch (error) {
                console.error(error);
                say("is-error", MeshI18n.t("The instance could not be added. Try again in a while."));
            } finally {
                button.disabled = false;
            }
        });
    }

    loadList();
    loadHistory();
})();
