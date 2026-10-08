// @category ReverseEngineering
// Locate exact scalar operands and decompile their containing functions.
import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.DecompInterface;
import ghidra.program.model.listing.*;
import ghidra.program.model.scalar.Scalar;
import java.util.HashSet;

public class FindScalarFunctions extends GhidraScript {
    public void run() throws Exception {
        long wanted = Long.decode(getScriptArgs()[0]);
        var done = new HashSet<Function>();
        var dc = new DecompInterface();
        dc.openProgram(currentProgram);
        var it = currentProgram.getListing().getInstructions(true);
        while (it.hasNext() && !monitor.isCancelled()) {
            Instruction ins = it.next();
            for (int i = 0; i < ins.getNumOperands(); i++) {
                for (Object op : ins.getOpObjects(i)) {
                    if (!(op instanceof Scalar) || ((Scalar)op).getUnsignedValue() != wanted)
                        continue;
                    Function fn = getFunctionContaining(ins.getAddress());
                    println("MATCH " + ins.getAddress() + " " + ins + " in " + fn);
                    if (fn != null && done.add(fn)) {
                        var out = dc.decompileFunction(fn, 60, monitor);
                        if (out.decompileCompleted()) println(out.getDecompiledFunction().getC());
                    }
                }
            }
        }
        dc.dispose();
    }
}
