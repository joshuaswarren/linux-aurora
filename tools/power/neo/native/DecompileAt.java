// @category ReverseEngineering
// Decompile the functions containing the supplied instruction addresses.
import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.DecompInterface;
public class DecompileAt extends GhidraScript {
    public void run() throws Exception {
        var dc = new DecompInterface();
        dc.openProgram(currentProgram);
        for (String arg : getScriptArgs()) {
            var fn = getFunctionContaining(toAddr(arg));
            println("ADDRESS " + arg + " FUNCTION " + fn);
            if (fn == null) continue;
            println("ENTRY " + fn.getEntryPoint());
            var out = dc.decompileFunction(fn, 120, monitor);
            if (out.decompileCompleted()) println(out.getDecompiledFunction().getC());
            else println(out.getErrorMessage());
        }
        dc.dispose();
    }
}
