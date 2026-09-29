"""Injected into the Wasm interpreter; host capabilities always enforce grants."""
class Hydra:
    def call(self, name, request=None):
        return _hydra_call(name, {} if request is None else request)

    def settings(self):
        return self.call("settings")

    def exec(self, program, args):
        return self.call("exec", {"program": program, "args": args})

    def prompt(self, form):
        return self.call("prompt", form)

    def log(self, level, message):
        return self.call("log", {"level": level, "message": message})

    def file(self, url, title="Download", id="file"):
        return {"plan": {"id": id, "title": title, "tracks": [
            {"id": id, "kind": "file", "sources": [{"url": url}]}]}}

hydra = Hydra()
