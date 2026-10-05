/* Injected by the guest runtime; all I/O goes through granted Hydra capabilities. */
const hydra = Object.freeze({
  call(name, request = {}) { return JSON.parse(_hydra_call(name, JSON.stringify(request))); },
  settings() { return this.call("settings"); },
  exec(program, args) { return this.call("exec", {program, args}); },
  prompt(form) { return this.call("prompt", form); },
  log(level, message) { return this.call("log", {level, message}); },
  file(url, title = "Download", id = "file") {
    return {plan: {id, title, tracks: [{id, kind: "file", sources: [{url}]}]}};
  }
});
