/* Reproduce the GL33 packed_depth_stencil.blit test and report each sample
 * pair. This keeps the CTS draw/blit sequence intact and reads twice only as
 * a diagnostic for the independent late-readback issue. */
#define GL_GLEXT_PROTOTYPES
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GL/gl.h>
#include <GL/glext.h>
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define W 256
#define CHECK(x) do { if (!(x)) { fprintf(stderr, "failed: %s line %d error=0x%x\n", #x, __LINE__, glGetError()); exit(2); } } while (0)

enum plane {
   PLANE_BOTH,
   PLANE_DEPTH,
   PLANE_STENCIL,
};

static GLuint
shader(GLenum stage, const char *source)
{
   GLuint s = glCreateShader(stage);
   glShaderSource(s, 1, &source, NULL);
   glCompileShader(s);
   GLint ok = 0;
   glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
   if (!ok) {
      char log[4096];
      glGetShaderInfoLog(s, sizeof(log), NULL, log);
      fprintf(stderr, "%s\n", log);
      exit(2);
   }
   return s;
}

static void
set_buffers(GLenum draw, GLenum read)
{
   glDrawBuffers(1, &draw);
   glReadBuffer(read);
   CHECK(glGetError() == GL_NO_ERROR);
}

static void
draw_quad(GLuint vbo, GLint position, int gradient)
{
   static const GLfloat flat[] = {
      -1,-1,0,1, 1,-1,0,1, -1,1,0,1, 1,1,0,1,
   };
   static const GLfloat span[] = {
      -1,-1,-1,1, 1,-1,-1,1, -1,1,1,1, 1,1,1,1,
   };
   glBindBuffer(GL_ARRAY_BUFFER, vbo);
   glBufferData(GL_ARRAY_BUFFER, gradient ? sizeof(span) : sizeof(flat),
                gradient ? span : flat, GL_STREAM_DRAW);
   glVertexAttribPointer(position, 4, GL_FLOAT, GL_FALSE, 0, NULL);
   glEnableVertexAttribArray(position);
   glDrawElements(GL_TRIANGLE_STRIP, 6, GL_UNSIGNED_SHORT, NULL);
   CHECK(glGetError() == GL_NO_ERROR);
}

static unsigned
check_image(const GLubyte *image, enum plane plane, uint64_t *hash,
            unsigned *white, unsigned *gray)
{
   unsigned bad = 0;
   *white = *gray = 0;
   *hash = UINT64_C(14695981039346656037);
   for (unsigned y = 0; y < W; ++y) {
      for (unsigned x = 0; x < W; ++x) {
         const GLubyte r = image[(y * W + x) * 4];
         *hash = (*hash ^ r) * UINT64_C(1099511628211);
         *white += r == 255;
         *gray += r == 204;
         if (plane == PLANE_STENCIL) {
            if (r != 255)
               ++bad;
         } else {
            if (y < W * 0.48 && r != 204)
               ++bad;
            else if (y > W * 0.52 && r != 255)
               ++bad;
         }
      }
   }
   return bad;
}

static unsigned
one(GLenum format, int source_samples, int destination_samples,
    enum plane plane, GLuint program, GLuint vao, GLuint vbo, GLint position)
{
   GLuint fbo[3], rbo[4];
   glGenFramebuffers(3, fbo);
   glGenRenderbuffers(4, rbo);

   glBindFramebuffer(GL_FRAMEBUFFER, fbo[0]);
   glBindRenderbuffer(GL_RENDERBUFFER, rbo[0]);
   glRenderbufferStorageMultisample(GL_RENDERBUFFER, source_samples, format, W, W);
   glFramebufferRenderbuffer(GL_FRAMEBUFFER, GL_DEPTH_STENCIL_ATTACHMENT,
                             GL_RENDERBUFFER, rbo[0]);
   set_buffers(GL_NONE, GL_NONE);
   CHECK(glCheckFramebufferStatus(GL_FRAMEBUFFER) == GL_FRAMEBUFFER_COMPLETE);

   glBindFramebuffer(GL_FRAMEBUFFER, fbo[1]);
   glBindRenderbuffer(GL_RENDERBUFFER, rbo[1]);
   glRenderbufferStorageMultisample(GL_RENDERBUFFER, destination_samples, format, W, W);
   glFramebufferRenderbuffer(GL_FRAMEBUFFER, GL_DEPTH_STENCIL_ATTACHMENT,
                             GL_RENDERBUFFER, rbo[1]);
   glBindRenderbuffer(GL_RENDERBUFFER, rbo[2]);
   glRenderbufferStorageMultisample(GL_RENDERBUFFER, destination_samples,
                                    GL_RGBA8, W, W);
   glFramebufferRenderbuffer(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0,
                             GL_RENDERBUFFER, rbo[2]);
   set_buffers(GL_COLOR_ATTACHMENT0, GL_COLOR_ATTACHMENT0);
   CHECK(glCheckFramebufferStatus(GL_FRAMEBUFFER) == GL_FRAMEBUFFER_COMPLETE);

   glBindFramebuffer(GL_FRAMEBUFFER, fbo[2]);
   glBindRenderbuffer(GL_RENDERBUFFER, rbo[3]);
   glRenderbufferStorage(GL_RENDERBUFFER, GL_RGBA8, W, W);
   glFramebufferRenderbuffer(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0,
                             GL_RENDERBUFFER, rbo[3]);
   set_buffers(GL_COLOR_ATTACHMENT0, GL_COLOR_ATTACHMENT0);
   CHECK(glCheckFramebufferStatus(GL_FRAMEBUFFER) == GL_FRAMEBUFFER_COMPLETE);

   glBindFramebuffer(GL_FRAMEBUFFER, fbo[0]);
   set_buffers(GL_NONE, GL_NONE);
   glViewport(0, 0, W, W);
   glEnable(GL_DEPTH_TEST);
   glDepthMask(GL_TRUE);
   glDepthFunc(GL_LEQUAL);
   glClearDepth(1.0);
   glEnable(GL_STENCIL_TEST);
   glStencilFunc(GL_ALWAYS, 1, 0xff);
   glStencilOp(GL_REPLACE, GL_REPLACE, GL_REPLACE);
   glClearStencil(0);
   glClearColor(0.8, 0.8, 0.8, 0.8);
   glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT | GL_STENCIL_BUFFER_BIT);
   draw_quad(vbo, position, 1);

   glBindFramebuffer(GL_READ_FRAMEBUFFER, fbo[0]);
   glBindFramebuffer(GL_DRAW_FRAMEBUFFER, fbo[1]);
   set_buffers(GL_NONE, GL_NONE);
   GLbitfield mask = plane == PLANE_DEPTH ? GL_DEPTH_BUFFER_BIT
                     : plane == PLANE_STENCIL ? GL_STENCIL_BUFFER_BIT
                                              : GL_DEPTH_BUFFER_BIT |
                                                   GL_STENCIL_BUFFER_BIT;
   glBlitFramebuffer(0, 0, W, W, 0, 0, W, W, mask, GL_NEAREST);
   CHECK(glGetError() == GL_NO_ERROR);

   glBindFramebuffer(GL_FRAMEBUFFER, fbo[1]);
   set_buffers(GL_COLOR_ATTACHMENT0, GL_COLOR_ATTACHMENT0);
   if (plane == PLANE_DEPTH)
      glDisable(GL_STENCIL_TEST);
   else if (plane == PLANE_STENCIL)
      glDisable(GL_DEPTH_TEST);
   glStencilFunc(GL_EQUAL, 1, 0xff);
   glStencilOp(GL_KEEP, GL_KEEP, GL_KEEP);
   glClear(GL_COLOR_BUFFER_BIT);
   draw_quad(vbo, position, 0);

   glBindFramebuffer(GL_READ_FRAMEBUFFER, fbo[1]);
   glBindFramebuffer(GL_DRAW_FRAMEBUFFER, fbo[2]);
   set_buffers(GL_COLOR_ATTACHMENT0, GL_COLOR_ATTACHMENT0);
   glBlitFramebuffer(0, 0, W, W, 0, 0, W, W, GL_COLOR_BUFFER_BIT, GL_NEAREST);
   CHECK(glGetError() == GL_NO_ERROR);

   GLubyte *a = malloc(W * W * 4), *b = malloc(W * W * 4);
   CHECK(a && b);
   glBindFramebuffer(GL_FRAMEBUFFER, fbo[2]);
   glReadPixels(0, 0, W, W, GL_RGBA, GL_UNSIGNED_BYTE, a);
   glReadPixels(0, 0, W, W, GL_RGBA, GL_UNSIGNED_BYTE, b);
   CHECK(glGetError() == GL_NO_ERROR);
   uint64_t hash_a, hash_b;
   unsigned white_a, gray_a, white_b, gray_b;
   unsigned bad_a = check_image(a, plane, &hash_a, &white_a, &gray_a);
   unsigned bad_b = check_image(b, plane, &hash_b, &white_b, &gray_b);
   const char *plane_name = plane == PLANE_DEPTH ? "depth"
                            : plane == PLANE_STENCIL ? "stencil"
                                                     : "both";
   printf("format=0x%x src=%d dst=%d plane=%s bad=%u second_bad=%u unstable=%u "
          "white=%u gray=%u second_white=%u second_gray=%u hash=%016" PRIx64
          " second_hash=%016" PRIx64 "\n", format, source_samples,
          destination_samples, plane_name, bad_a, bad_b,
          memcmp(a, b, W * W * 4) != 0,
          white_a, gray_a, white_b, gray_b, hash_a, hash_b);

   free(a);
   free(b);
   glBindFramebuffer(GL_FRAMEBUFFER, 0);
   glDeleteRenderbuffers(4, rbo);
   glDeleteFramebuffers(3, fbo);
   (void)program;
   (void)vao;
   return bad_b != 0;
}

int
main(void)
{
   const char *plane_env = getenv("PROBE_PLANE");
   enum plane plane = PLANE_BOTH;
   if (plane_env && !strcmp(plane_env, "depth"))
      plane = PLANE_DEPTH;
   else if (plane_env && !strcmp(plane_env, "stencil"))
      plane = PLANE_STENCIL;
   else if (plane_env && strcmp(plane_env, "both")) {
      fprintf(stderr, "PROBE_PLANE must be both, depth, or stencil\n");
      return 2;
   }

   const char *format_env = getenv("PROBE_FORMAT");
   const char *src_env = getenv("PROBE_SRC");
   const char *dst_env = getenv("PROBE_DST");
   const unsigned selected_format = format_env ? strtoul(format_env, NULL, 0) : 0;
   const int selected_src = src_env ? strtol(src_env, NULL, 0) : -1;
   const int selected_dst = dst_env ? strtol(dst_env, NULL, 0) : -1;

   EGLDisplay display = eglGetDisplay(EGL_DEFAULT_DISPLAY);
   CHECK(display != EGL_NO_DISPLAY && eglInitialize(display, NULL, NULL));
   CHECK(eglBindAPI(EGL_OPENGL_API));
   EGLint config_attrs[] = {EGL_SURFACE_TYPE, EGL_PBUFFER_BIT,
                            EGL_RENDERABLE_TYPE, EGL_OPENGL_BIT,
                            EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8,
                            EGL_BLUE_SIZE, 8, EGL_NONE};
   EGLConfig config;
   EGLint count;
   CHECK(eglChooseConfig(display, config_attrs, &config, 1, &count) && count);
   EGLint context_attrs[] = {EGL_CONTEXT_MAJOR_VERSION, 3,
                             EGL_CONTEXT_MINOR_VERSION, 3,
                             EGL_CONTEXT_OPENGL_PROFILE_MASK,
                             EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT, EGL_NONE};
   EGLContext context = eglCreateContext(display, config, EGL_NO_CONTEXT,
                                         context_attrs);
   EGLint surface_attrs[] = {EGL_WIDTH, W, EGL_HEIGHT, W, EGL_NONE};
   EGLSurface surface = eglCreatePbufferSurface(display, config, surface_attrs);
   CHECK(context != EGL_NO_CONTEXT && surface != EGL_NO_SURFACE &&
         eglMakeCurrent(display, surface, surface, context));
   printf("renderer=%s\n", glGetString(GL_RENDERER));

   GLuint vs = shader(GL_VERTEX_SHADER,
      "#version 330 core\nlayout(location=0) in vec4 pos;"
      "void main(){gl_Position=pos;}");
   GLuint fs = shader(GL_FRAGMENT_SHADER,
      "#version 330 core\nout vec4 color;"
      "void main(){color=vec4(1);}");
   GLuint program = glCreateProgram();
   glAttachShader(program, vs);
   glAttachShader(program, fs);
   glLinkProgram(program);
   GLint linked = 0;
   glGetProgramiv(program, GL_LINK_STATUS, &linked);
   CHECK(linked);
   glUseProgram(program);
   GLuint vao, vbo, ibo;
   static const GLushort indices[] = {0,1,2,2,1,3};
   glGenVertexArrays(1, &vao);
   glBindVertexArray(vao);
   glGenBuffers(1, &vbo);
   glGenBuffers(1, &ibo);
   glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, ibo);
   glBufferData(GL_ELEMENT_ARRAY_BUFFER, sizeof(indices), indices,
                GL_STATIC_DRAW);

   GLint max_samples = 0;
   glGetIntegerv(GL_MAX_SAMPLES, &max_samples);
   printf("max_samples=%d\n", max_samples);
   const GLenum formats[] = {GL_DEPTH24_STENCIL8, GL_DEPTH32F_STENCIL8};
   unsigned failures = 0;
   for (unsigned f = 0; f < 2; ++f)
      for (int direction = 0; direction < 2; ++direction)
         for (int samples = 0; samples <= max_samples; ++samples) {
            const int src = direction ? 0 : samples;
            const int dst = direction ? samples : 0;
            if ((selected_format && selected_format != formats[f]) ||
                (selected_src >= 0 && selected_src != src) ||
                (selected_dst >= 0 && selected_dst != dst))
               continue;
            failures += one(formats[f], src, dst, plane, program, vao, vbo, 0);
         }

   glDeleteBuffers(1, &vbo);
   glDeleteBuffers(1, &ibo);
   glDeleteVertexArrays(1, &vao);
   glDeleteProgram(program);
   glDeleteShader(vs);
   glDeleteShader(fs);
   eglMakeCurrent(display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
   eglDestroySurface(display, surface);
   eglDestroyContext(display, context);
   eglTerminate(display);
   return failures != 0;
}
