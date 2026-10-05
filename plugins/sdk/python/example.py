def resolve(request):
    settings = hydra.settings()
    return hydra.file(request["url"], settings.get("title", "Python example"))
