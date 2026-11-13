/* Apply before paint; one preference shared by every Meshview page. */
(() => {
    const root = document.documentElement;
    const key = 'meshview-theme';
    const system = matchMedia('(prefers-color-scheme: dark)');
    let preference = null;
    try { preference = localStorage.getItem(key); } catch {}
    if (!['light','dark'].includes(preference)) preference = null;
    root.dataset.bsTheme = preference || (system.matches ? 'dark' : 'light');

    function updateButton() {
        document.querySelectorAll('[data-theme-toggle]').forEach(button => {
            const dark = root.dataset.bsTheme === 'dark';
            button.setAttribute('aria-label',dark ? MeshI18n.t("Switch to day mode") : MeshI18n.t("Switch to night mode"));
            button.setAttribute('aria-pressed',String(dark));
            button.title = dark ? MeshI18n.t("Day mode") : MeshI18n.t("Night mode");
        });
    }
    function apply(theme) {
        root.dataset.bsTheme = theme;
        updateButton();
        // Whatever draws in colours of its own, as the chart, follows this.
        document.dispatchEvent(new CustomEvent('meshview:themechange',{detail:theme}));
    }
    document.addEventListener('meshview:languagechange', updateButton);
    document.addEventListener('DOMContentLoaded',() => {
        updateButton();
        document.querySelectorAll('[data-theme-toggle]').forEach(button => {
            button.addEventListener('click',() => {
                preference = root.dataset.bsTheme === 'dark' ? 'light' : 'dark';
                try { localStorage.setItem(key,preference); } catch {}
                apply(preference);
            });
        });
    });
    system.addEventListener('change',event => {
        if (!preference) apply(event.matches ? 'dark' : 'light');
    });
    window.addEventListener('storage',event => {
        if (event.key !== key && event.key !== null) return;
        preference = ['light','dark'].includes(event.newValue) ? event.newValue : null;
        apply(preference || (system.matches ? 'dark' : 'light'));
    });
})();
