class _CelldModule:
    pass

_celld_loaded = set()
_celld_loading = set()


def _celld_import(name):
    module, initialize = _celld_modules[name]
    if name in _celld_loaded or name in _celld_loading:
        return module
    parent = None
    if '.' in name:
        parent_name, child = name.rsplit('.', 1)
        parent = _celld_import(parent_name)
        if name in _celld_loaded:
            return module
    _celld_loading.add(name)
    try:
        initialize()
        _celld_loaded.add(name)
        if parent is not None:
            setattr(parent, child, module)
    finally:
        _celld_loading.remove(name)
    return module


def _celld_from(name, member):
    module = _celld_import(name)
    if hasattr(module, member):
        return getattr(module, member)
    if name + '.' + member in _celld_modules:
        return _celld_import(name + '.' + member)
    raise ImportError('cannot import ' + member + ' from ' + name)
