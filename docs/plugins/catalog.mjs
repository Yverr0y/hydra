export function validateCatalog(catalog) {
  if (!catalog || catalog.schema !== 1 || typeof catalog.name !== 'string' || !catalog.name.trim()) {
    throw new Error('Invalid plugin catalog');
  }
  const plugins = catalog.plugins;
  if (!Array.isArray(plugins)) throw new Error('Invalid plugin catalog');
  const ids = new Set();
  for (const plugin of plugins) {
    if (!plugin || ['id', 'name', 'description', 'version', 'author', 'image', 'download', 'homepage']
      .some(key => typeof plugin[key] !== 'string' || !plugin[key].trim()) ||
      typeof plugin.is_official !== 'boolean' ||
      (plugin.sha256 != null && (typeof plugin.sha256 !== 'string' || !/^[a-f0-9]{64}$/i.test(plugin.sha256))) ||
      (plugin.publisher_key != null && (typeof plugin.publisher_key !== 'string' || !plugin.publisher_key.trim())) ||
      (!Number.isInteger(plugin.api) || plugin.api < 1) || ids.has(plugin.id) ||
      !/^[a-z0-9]+(?:[.-][a-z0-9]+)*$/.test(plugin.id)) {
      throw new Error('Invalid plugin details');
    }
    for (const key of ['download', 'homepage']) {
      const url = new URL(plugin[key]);
      if (url.protocol !== 'https:' || url.username || url.password || url.hash) {
        throw new Error('Plugin links must use HTTPS');
      }
      if (key === 'download' && !url.pathname.toLowerCase().endsWith('.hyaplugin')) {
        throw new Error('Invalid plugin package');
      }
    }
    if (!plugin.image.startsWith(`plugins/img/${plugin.id}/`) ||
        plugin.image.includes('..') || /[?#\\]/.test(plugin.image)) {
      throw new Error('Invalid plugin image');
    }
    ids.add(plugin.id);
  }
  return plugins;
}

export function searchPlugins(plugins, query) {
  const name = query.trim().toLowerCase();
  return plugins.filter(plugin => plugin.name.toLowerCase().includes(name));
}

export function installLink(plugin) {
  return `hydra://install-plugin?url=${encodeURIComponent(plugin.download)}`;
}
