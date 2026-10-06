import { validateCatalog, searchPlugins, installLink } from './catalog.mjs';

const grid = document.getElementById('plugin-list');
const count = document.getElementById('plugin-count');
const search = document.getElementById('plugin-search');
const state = document.getElementById('catalog-state');
const retry = document.getElementById('catalog-retry');
const installStatus = document.getElementById('install-status');
let plugins = [];

function element(tag, className, text) {
  const node = document.createElement(tag);
  node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function pluginCard(plugin) {
  const card = element('article', 'plugin-card');
  const header = element('div', 'card-header');
  const image = element('img', 'plugin-image');
  image.src = plugin.image;
  image.alt = '';
  image.width = 72;
  image.height = 72;
  const heading = element('div', 'card-heading');
  heading.append(element('span', `badge ${plugin.is_official ? 'official' : ''}`,
    plugin.is_official ? 'Official' : 'Community'));
  heading.append(element('h2', '', plugin.name));
  header.append(image, heading);
  const details = element('dl', 'plugin-details');
  for (const [label, value] of [['Version', plugin.version], ['Author', plugin.author]]) {
    const row = element('div', 'detail-row');
    row.append(element('dt', '', label), element('dd', '', value));
    details.append(row);
  }
  const homepage = element('a', 'homepage', 'Plugin homepage ↗');
  homepage.href = plugin.homepage;
  const actions = element('div', 'card-actions');
  const download = element('a', 'button secondary', 'Download .hyaplugin');
  download.href = plugin.download;
  const install = element('a', 'button primary', 'Install in Hydra');
  install.href = installLink(plugin);
  install.addEventListener('click', () => {
    installStatus.textContent = `Your browser will ask to open Hydra to review ${plugin.name}. If Hydra does not open, download the .hyaplugin file and open it with Hydra, or use Options → Extensions & Plugins → Plugins.`;
  });
  actions.append(download, install);
  card.append(header, element('p', 'description', plugin.description), details, homepage, actions);
  return card;
}

function render() {
  const matches = searchPlugins(plugins, search.value);
  grid.replaceChildren(...matches.map(pluginCard));
  count.textContent = search.value.trim()
    ? `${matches.length} of ${plugins.length} plugins`
    : `${plugins.length} ${plugins.length === 1 ? 'plugin' : 'plugins'}`;
  state.hidden = matches.length > 0;
  state.textContent = plugins.length === 0 ? 'No plugins are available yet.' : 'No plugins match your search.';
}

async function load() {
  search.disabled = true;
  retry.hidden = true;
  state.hidden = false;
  state.textContent = 'Loading plugins…';
  try {
    const response = await fetch('plugins.json');
    if (!response.ok) throw new Error('Cannot load plugin catalog');
    plugins = validateCatalog(await response.json());
    search.disabled = false;
    render();
  } catch {
    grid.replaceChildren();
    count.textContent = 'Catalog unavailable';
    state.textContent = 'The plugin catalog could not be loaded. Please try again.';
    retry.hidden = false;
  }
}

search.addEventListener('input', render);
retry.addEventListener('click', load);
const navToggle = document.getElementById('nav-toggle');
const navLinks = document.getElementById('nav-links');
navToggle.addEventListener('click', () => {
  const open = navLinks.classList.toggle('open');
  navToggle.setAttribute('aria-expanded', String(open));
});
navLinks.addEventListener('click', event => {
  if (event.target.closest('a')) {
    navLinks.classList.remove('open');
    navToggle.setAttribute('aria-expanded', 'false');
  }
});
load();
