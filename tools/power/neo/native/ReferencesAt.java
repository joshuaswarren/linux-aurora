// @category ReverseEngineering
// Report existing references without modifying the analyzed firmware.
import ghidra.app.script.GhidraScript;
public class ReferencesAt extends GhidraScript {
    public void run() throws Exception {
        for (String arg : getScriptArgs()) {
            var addr = toAddr(arg);
            println("TARGET " + addr);
            var refs = currentProgram.getReferenceManager().getReferencesTo(addr);
            while (refs.hasNext() && !monitor.isCancelled()) {
                var ref = refs.next();
                var from = ref.getFromAddress();
                println("FROM " + from + " TYPE " + ref.getReferenceType() +
                        " FUNCTION " + getFunctionContaining(from));
            }
        }
    }
}
