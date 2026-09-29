function resolve(request) {
  const settings = hydra.settings();
  return hydra.file(request.url, settings.title || "JavaScript example");
}
