;; Fixture `core-module`: a plain core wasm module, not a component. The
;; loader and the registry upload must both reject it and say why.
;; core_module.wasm beside this file is this text assembled (committed so the
;; build script needs no wat tool); regenerate it if you edit this file.
(module
  (func (export "run") (result i32)
    i32.const 42))
