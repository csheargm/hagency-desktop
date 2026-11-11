use makepad_script::*;

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().expect("parse, eval or root");
    let path = args.next().expect("source path");
    assert!(args.next().is_none(), "unexpected argument");
    let source = std::fs::read_to_string(&path).expect("read script");
    match mode.as_str() {
        "parse" => {
            let mut heap = heap::ScriptHeap::default();
            let mut tokenizer = tokenizer::ScriptTokenizer::default();
            tokenizer.tokenize(&source, &mut heap);
            tokenizer.tokenize("\n;", &mut heap);
            let mut parser = parser::ScriptParser::default();
            parser.parse(&tokenizer, &path, (0, 0), &[]);
            assert!(parser.diagnostics().is_empty(), "{:?}", parser.diagnostics());
        }
        "eval" | "root" => {
            let mut host = ScriptVmHost::new((), ());
            let mut vm = ScriptVm {
                host: host.host_context(), // JIT PORT: the VM borrows a host context, not the host
                bx: Box::new(ScriptVmBase::new()),
            };
            vm.bx.captured_errors = Some(Vec::new());
            let result = vm.eval(ScriptMod {
                file: path,
                code: source + "\n;",
                ..Default::default()
            });
            let errors = vm.take_errors();
            assert!(errors.is_empty(), "{errors:?}");
            if mode == "root" {
                let object = result.as_object().expect("root must be a widget object");
                let children = vm.vec_with(object, |_vm, values| {
                    values.iter().map(|v| v.key.as_id().expect("named root child").to_string()).collect::<Vec<_>>()
                });
                assert_eq!(children, ["shell", "alert_layer"], "full bundle lost or misplaced root children");
                let shell = vm.vec_with(object, |_vm, values| values[0].value.as_object().expect("shell widget"));
                let shell_children = vm.vec_with(shell, |_vm, values| {
                    values.iter().map(|v| v.key.as_id().expect("named shell child").to_string()).collect::<Vec<_>>()
                });
                assert_eq!(shell_children, ["heading", "navigation", "page_bar", "notice", "content"], "full bundle lost or misplaced shell children");
                println!("root children: {}", children.join(", "));
            } else {
                assert_eq!(result.as_bool(), Some(true), "{result:?}");
            }
        }
        _ => panic!("expected parse, eval or root"),
    }
    println!("{mode} passed");
}
