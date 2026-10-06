export function validateCatalog(catalog) {
  if (!catalog || catalog.schema !== 1 || typeof catalog.name !== 'string' || !catalog.name.trim()) {
    throw new Error('Invalid plugin catalog');
  }
  const plugins = catalog.plugins;
  if (!Array.isArray(plugins)) throw new Error('Invalid plugin catalog');
  const ids = new Set();
  for (const plugin of plugins) {
    if (!plugin || ['id', 'name', 'description', 'version', 'author', 'image', 'homepage']
      .some(key => typeof plugin[key] !== 'string' || !plugin[key].trim()) ||
      typeof plugin.is_official !== 'boolean' ||
      (plugin.sha256 != null && (typeof plugin.sha256 !== 'string' || !/^[a-f0-9]{64}$/i.test(plugin.sha256))) ||
      (plugin.publisher_key != null && (typeof plugin.publisher_key !== 'string' || !plugin.publisher_key.trim())) ||
      (!Number.isInteger(plugin.api) || plugin.api < 1) || ids.has(plugin.id) ||
      !/^[a-z0-9]+(?:[.-][a-z0-9]+)*$/.test(plugin.id)) {
      throw new Error('Invalid plugin details');
    }
    const downloads = downloadChoices(plugin);
    if (!downloads.length) throw new Error('Plugin needs a download');
    for (const choice of downloads) {
      if (!choice || typeof choice.caption !== 'string' || !choice.caption.trim() ||
          typeof choice.link !== 'string' || !choice.link.trim() ||
          (choice.platform != null && (typeof choice.platform !== 'string' || !choice.platform.trim())) ||
          (choice.sha256 != null && (typeof choice.sha256 !== 'string' || !/^[a-f0-9]{64}$/i.test(choice.sha256)))) {
        throw new Error('Invalid plugin download');
      }
    }
    for (const [key, address] of [['homepage', plugin.homepage], ...downloads.map(choice => ['download', choice.link])]) {
      const url = new URL(address);
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

export function downloadChoices(plugin) {
  if (typeof plugin.download === 'string') return [{ caption: 'All platforms', link: plugin.download }];
  if (!Array.isArray(plugin.download)) throw new Error('Invalid plugin downloads');
  return plugin.download;
}

export function installLink(download) {
  return `hydra://install-plugin?url=${encodeURIComponent(download.link)}`;
}
