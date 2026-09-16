/* Churn compressed textures the way packed_pixels does and look for a
 * readback that differs from the first, stable result. */
#include <EGL/egl.h>
#include <GL/gl.h>
#include <GL/glext.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static const GLenum ifmts[] = {
   0x8225 /* COMPRESSED_RED */,  0x8226 /* COMPRESSED_RG */,
   0x84ED /* COMPRESSED_RGB */,  0x84EE /* COMPRESSED_RGBA */,
   0x8DBB /* RED_RGTC1 */,       0x8DBD /* RG_RGTC2 */,
};

int main(void)
{
   EGLDisplay d = eglGetDisplay(EGL_DEFAULT_DISPLAY);
   EGLint a, b;
   if (d == EGL_NO_DISPLAY || !eglInitialize(d, &a, &b) ||
       !eglBindAPI(EGL_OPENGL_API)) {
      fprintf(stderr, "loop: EGL initialization failed: 0x%x\n", eglGetError());
      return 2;
   }
   EGLint ca[] = {EGL_SURFACE_TYPE, EGL_PBUFFER_BIT, EGL_RENDERABLE_TYPE,
                  EGL_OPENGL_BIT, EGL_NONE};
   EGLConfig cfg; EGLint n;
   if (!eglChooseConfig(d, ca, &cfg, 1, &n) || n != 1) {
      fprintf(stderr, "loop: EGL config selection failed: 0x%x\n", eglGetError());
      return 2;
   }
   EGLint xa[] = {EGL_CONTEXT_MAJOR_VERSION, 3, EGL_CONTEXT_MINOR_VERSION, 3,
                  EGL_CONTEXT_OPENGL_PROFILE_MASK,
                  EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT, EGL_NONE};
   EGLContext ctx = eglCreateContext(d, cfg, EGL_NO_CONTEXT, xa);
   EGLint sa[] = {EGL_WIDTH, 16, EGL_HEIGHT, 16, EGL_NONE};
   EGLSurface s = eglCreatePbufferSurface(d, cfg, sa);
   if (ctx == EGL_NO_CONTEXT || s == EGL_NO_SURFACE ||
       !eglMakeCurrent(d, s, s, ctx)) {
      fprintf(stderr, "loop: EGL context setup failed: 0x%x\n", eglGetError());
      return 2;
   }
   printf("loop: renderer %s\n", (const char *)glGetString(GL_RENDERER));

   const int iters = getenv("LOOP_N") ? atoi(getenv("LOOP_N")) : 300;
   const int fail_fast = getenv("LOOP_FAIL_FAST") &&
                         strcmp(getenv("LOOP_FAIL_FAST"), "1") == 0;
   if (iters < 24) {
      fprintf(stderr, "loop: at least 24 iterations are required\n");
      return 2;
   }
   const int sizes[] = {10, 15};
   unsigned char ref[6][2][256];
   int have_ref[6][2];
   memset(have_ref, 0, sizeof(have_ref));
   unsigned bad = 0, checked = 0, unstable_read = 0, second_ok = 0;
   unsigned executed = 0;

   glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
   glPixelStorei(GL_PACK_ALIGNMENT, 1);

   for (int it = 0; it < iters; ++it) {
      unsigned fi = it % 6, si = (it / 6) % 2;
      int W = sizes[si], H = W;
      unsigned char src[256 * 4];
      for (int y = 0; y < H; ++y)
         for (int x = 0; x < W; ++x)
            for (int c = 0; c < 4; ++c)
               src[(y * W + x) * 4 + c] = (unsigned char)((x * 255) / (W - 1));

      GLuint t;
      glGenTextures(1, &t);
      glBindTexture(GL_TEXTURE_2D, t);
      glTexImage2D(GL_TEXTURE_2D, 0, ifmts[fi], W, H, 0, GL_RGBA,
                   GL_UNSIGNED_BYTE, src);
      if (getenv("LOOP_FINISH")) glFinish();

      unsigned char out[256], out2[256];
      memset(out, 0xEE, sizeof(out));
      memset(out2, 0xEE, sizeof(out2));
      glGetTexImage(GL_TEXTURE_2D, 0, GL_RED, GL_UNSIGNED_BYTE, out);
      glGetTexImage(GL_TEXTURE_2D, 0, GL_RED, GL_UNSIGNED_BYTE, out2);
      if (memcmp(out, out2, W * H) != 0) ++unstable_read;

      if (!have_ref[fi][si]) {
         memcpy(ref[fi][si], out, W * H);
         have_ref[fi][si] = 1;
      } else {
         ++checked;
         if (memcmp(ref[fi][si], out, W * H) != 0) {
            ++bad;
            if (memcmp(ref[fi][si], out2, W * H) == 0) ++second_ok;
            if (bad <= 3) {
               printf("loop: MISMATCH iter=%d ifmt=0x%04x %dx%d\n  ref=", it,
                      ifmts[fi], W, H);
               for (int i = 0; i < W; ++i) printf("%02x ", ref[fi][si][i]);
               printf("\n  got=");
               for (int i = 0; i < W; ++i) printf("%02x ", out[i]);
               printf("\n");
            }
         }
      }
      if (!getenv("LOOP_KEEP")) glDeleteTextures(1, &t);
      ++executed;
      /* An observed error is sufficient to reject a candidate. Successful
       * runs still execute the entire requested budget, with identical GL
       * calls and no added synchronization between the two reads. */
      if (fail_fast && (bad || unstable_read)) break;
   }
   printf("loop: iterations=%u requested=%d fail_fast=%d\n",
          executed, iters, fail_fast);
   printf("loop: checked=%u mismatches=%u unstable_read=%u second_read_ok=%u\n",
          checked, bad, unstable_read, second_ok);
   /* GL errors are sticky. Check once after the timed loop so an API failure
    * cannot turn untouched output buffers into a false pass, without adding
    * any calls between the reads being compared. */
   GLenum error = glGetError();
   if (error != GL_NO_ERROR) {
      fprintf(stderr, "loop: GL error during probe: 0x%x\n", error);
      return 2;
   }
   return bad != 0 || unstable_read != 0;
}
