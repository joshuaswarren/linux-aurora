/* Diagnostic-only buffer/fence history. Never included in production Mesa. */
struct agx_context;
struct agx_batch;
struct agx_bo;

struct readback_journal_event {
   const char *kind;
   uint64_t va, value, size;
   uint32_t bo, syncobj, batch;
   int result;
};

void agx_readback_record(struct agx_context *ctx, const char *kind,
                         struct agx_batch *batch, struct agx_bo *bo,
                         uint64_t value, uint64_t size, int result);
void agx_readback_dump(struct agx_context *ctx, struct agx_bo *bo);
