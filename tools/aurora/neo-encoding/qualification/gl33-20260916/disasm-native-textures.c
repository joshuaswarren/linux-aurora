#include <stdint.h>
#include <stdbool.h>
#include "asahi/isa/disasm.h"

int main(int argc, char **argv)
{
   if (argc != 2)
      return 2;
   FILE *f = fopen(argv[1], "rb");
   if (!f || fseek(f, 0, SEEK_END))
      return 2;
   long length = ftell(f);
   if (length <= 0 || length > 1024 * 1024 || fseek(f, 0, SEEK_SET))
      return 2;
   uint8_t *code = calloc(1, length + 16);
   if (!code || fread(code, 1, length, f) != (size_t)length)
      return 2;
   fclose(f);
   bool errors = agx3b_disassemble(code, length, stdout);
   free(code);
   return errors ? 1 : 0;
}
