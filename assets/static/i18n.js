/* Translate application copy only, as Meshview does. Node names, instance
   data and anything else from the meshes never enter this lookup. The page
   carries its catalog in window.meshTranslations. */
(() => {
    const catalog = window.meshTranslations || {};
    const language = window.meshLanguage || 'en';
    function t(source, values = {}) {
        const text = Object.hasOwn(catalog, source) && typeof catalog[source] === 'string'
            ? catalog[source] : source;
        return text.replace(/\{(\w+)\}/g, (match, key) =>
            Object.hasOwn(values, key) ? String(values[key]) : match);
    }
    function h(source, values = {}) {
        return escapeHtml(t(source, values));
    }
    function escapeHtml(value) {
        return String(value ?? '').replace(/[&<>"']/g, char => ({
            '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;'
        }[char]));
    }
    window.escapeHtml = escapeHtml;
    window.MeshI18n = {t, h, get language() { return language; }};
})();
