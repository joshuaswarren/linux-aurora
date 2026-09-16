/* Diagnostic: preserve one draw per case and compare restart with the same
 * complete primitives in a token-free index buffer. Run on Asahi and llvmpipe.
 */
#define GL_GLEXT_PROTOTYPES
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GL/gl.h>
#include <GL/glext.h>
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "failed: %s at %d\n", #x, __LINE__); exit(2); } } while (0)
static GLuint shader(GLenum stage, const char *source)
{
   GLuint s = glCreateShader(stage);
   glShaderSource(s, 1, &source, NULL);
   glCompileShader(s);
   GLint ok;
   glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
   if (!ok) {
      char log[4096];
      glGetShaderInfoLog(s, sizeof(log), NULL, log);
      fprintf(stderr, "%s\n", log);
      exit(2);
   }
   return s;
}

static uint64_t render(GLenum mode, const GLuint *indices, unsigned count,
                       int restart, int indirect, unsigned *lit)
{
   glClear(GL_COLOR_BUFFER_BIT);
   glBufferData(GL_ELEMENT_ARRAY_BUFFER, count * sizeof(*indices), indices,
                GL_STATIC_DRAW);
   if (restart) glEnable(GL_PRIMITIVE_RESTART);
   else glDisable(GL_PRIMITIVE_RESTART);
   glPrimitiveRestartIndex(~0u);
   if (indirect) {
      GLuint buffer;
      const GLuint command[] = {count, 1, 0, 0, 0};
      glGenBuffers(1, &buffer);
      glBindBuffer(GL_DRAW_INDIRECT_BUFFER, buffer);
      glBufferData(GL_DRAW_INDIRECT_BUFFER, sizeof(command), command,
                   GL_STATIC_DRAW);
      glDrawElementsIndirect(mode, GL_UNSIGNED_INT, NULL);
      glBindBuffer(GL_DRAW_INDIRECT_BUFFER, 0);
      glDeleteBuffers(1, &buffer);
   } else {
      glDrawElements(mode, count, GL_UNSIGNED_INT, NULL);
   }
   CHECK(glGetError() == GL_NO_ERROR);
   unsigned char image[64 * 64 * 4];
   glReadPixels(0, 0, 64, 64, GL_RGBA, GL_UNSIGNED_BYTE, image);
   CHECK(glGetError() == GL_NO_ERROR);
   uint64_t hash = UINT64_C(14695981039346656037);
   *lit = 0;
   for (unsigned i = 0; i < sizeof(image); ++i) {
      hash = (hash ^ image[i]) * UINT64_C(1099511628211);
      if (i % 4 == 0 && image[i]) ++*lit;
   }
   return hash;
}

int main(void)
{
   EGLDisplay d = eglGetDisplay(EGL_DEFAULT_DISPLAY);
   CHECK(d != EGL_NO_DISPLAY && eglInitialize(d, NULL, NULL));
   CHECK(eglBindAPI(EGL_OPENGL_API));
   EGLint attrs[] = {EGL_SURFACE_TYPE, EGL_PBUFFER_BIT, EGL_RENDERABLE_TYPE,
                    EGL_OPENGL_BIT, EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8,
                    EGL_BLUE_SIZE, 8, EGL_NONE};
   EGLConfig cfg;
   EGLint n;
   CHECK(eglChooseConfig(d, attrs, &cfg, 1, &n) && n);
   EGLint ca[] = {EGL_CONTEXT_MAJOR_VERSION, 3, EGL_CONTEXT_MINOR_VERSION, 3,
                 EGL_CONTEXT_OPENGL_PROFILE_MASK,
                 EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT, EGL_NONE};
   EGLContext c = eglCreateContext(d, cfg, EGL_NO_CONTEXT, ca);
   CHECK(c != EGL_NO_CONTEXT);
   EGLint sa[] = {EGL_WIDTH, 64, EGL_HEIGHT, 64, EGL_NONE};
   EGLSurface s = eglCreatePbufferSurface(d, cfg, sa);
   CHECK(s != EGL_NO_SURFACE && eglMakeCurrent(d, s, s, c));
   printf("renderer=%s\n", glGetString(GL_RENDERER));
   GLuint program = glCreateProgram();
   GLuint vs = shader(GL_VERTEX_SHADER, "#version 330 core\nlayout(location=0) in vec2 p; void main(){gl_Position=vec4(p,0,1);}");
   GLuint fs = shader(GL_FRAGMENT_SHADER, "#version 330 core\nout vec4 color; void main(){color=vec4(1);}");
   glAttachShader(program, vs);
   glAttachShader(program, fs);
   glLinkProgram(program);
   GLint ok;
   glGetProgramiv(program, GL_LINK_STATUS, &ok);
   CHECK(ok);
   glUseProgram(program);
   GLuint vao, vbo, ibo, tex, fb;
   glGenVertexArrays(1, &vao);
   glBindVertexArray(vao);
   glGenBuffers(1, &vbo);
   glBindBuffer(GL_ARRAY_BUFFER, vbo);
   const GLfloat vertices[] = {-.8f,-.8f, .8f,-.8f, -.8f,.8f, .8f,.8f, 0,0};
   glBufferData(GL_ARRAY_BUFFER, sizeof(vertices), vertices, GL_STATIC_DRAW);
   glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, NULL);
   glEnableVertexAttribArray(0);
   glGenBuffers(1, &ibo);
   glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, ibo);
   glGenTextures(1, &tex);
   glBindTexture(GL_TEXTURE_2D, tex);
   glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA8, 64, 64, 0, GL_RGBA,
                GL_UNSIGNED_BYTE, NULL);
   glGenFramebuffers(1, &fb);
   glBindFramebuffer(GL_FRAMEBUFFER, fb);
   glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D,
                          tex, 0);
   CHECK(glCheckFramebufferStatus(GL_FRAMEBUFFER) == GL_FRAMEBUFFER_COMPLETE);
   glViewport(0, 0, 64, 64);
   glClearColor(0, 0, 0, 1);
   glDisable(GL_DITHER);
   const GLuint lines[] = {4,4,3,3, 4,4,0,0};
   const GLuint lines_restart[] = {4,4,3,3, ~0u, 4,4,0,0};
   const GLuint tris[] = {0,0,1,1,2,2, 2,2,1,1,3,3};
   const GLuint tris_restart[] = {0,0,1,1,2,2, ~0u, 2,2,1,1,3,3};
   for (unsigned t = 0; t < 2; ++t) {
      GLenum mode = t ? GL_TRIANGLES_ADJACENCY : GL_LINES_ADJACENCY;
      const GLuint *plain_indices = t ? tris : lines;
      unsigned count = t ? 12 : 8;
      for (unsigned variant = 0; variant < 5; ++variant) {
         GLuint indices[16];
         unsigned n = count;
         memcpy(indices, plain_indices, count * sizeof(GLuint));
         if (variant == 1) {
            memmove(indices + 1, indices, count * sizeof(GLuint));
            indices[0] = ~0u;
            n++;
         } else if (variant == 2) {
            indices[n++] = ~0u;
         } else if (variant == 3) {
            n = count / 2;
         } else if (variant == 4) {
            n = count / 2;
            memmove(indices, indices + n, n * sizeof(GLuint));
         }
         unsigned lit;
         uint64_t hash = render(mode, indices, n, 1, 0, &lit);
         printf("variant=%u mode=%s hash=%016" PRIx64 " lit=%u\n",
                variant, t ? "triangles_adj" : "lines_adj", hash, lit);
      }
   }
   unsigned failures = 0;
   const unsigned repetitions[] = {9, 256, 257};
   for (unsigned t = 0; t < 2; ++t) {
      GLenum mode = t ? GL_TRIANGLES_ADJACENCY : GL_LINES_ADJACENCY;
      const GLuint *base = t ? tris : lines;
      unsigned per_prim = t ? 6 : 4;
      for (unsigned r = 0; r < sizeof(repetitions) / sizeof(repetitions[0]); ++r) {
         unsigned segment = per_prim * repetitions[r];
         GLuint *plain = malloc(2 * segment * sizeof(GLuint));
         GLuint *with_restart = malloc((2 * segment + 1) * sizeof(GLuint));
         CHECK(plain && with_restart);
         for (unsigned i = 0; i < segment; ++i) {
            plain[i] = base[i % per_prim];
            plain[segment + i] = base[per_prim + i % per_prim];
         }
         memcpy(with_restart, plain, segment * sizeof(GLuint));
         with_restart[segment] = ~0u;
         memcpy(with_restart + segment + 1, plain + segment, segment * sizeof(GLuint));
         for (unsigned indirect = 0; indirect < 2; ++indirect) {
            unsigned plain_lit, restart_lit;
            uint64_t a = render(mode, plain, 2 * segment, 0, indirect, &plain_lit);
            uint64_t b = render(mode, with_restart, 2 * segment + 1, 1, indirect, &restart_lit);
            printf("long mode=%s segment=%u indirect=%u plain=%016" PRIx64
                   " lit=%u restart=%016" PRIx64 " lit=%u equal=%u\n",
                   t ? "triangles_adj" : "lines_adj", segment, indirect,
                   a, plain_lit, b, restart_lit, a == b);
            failures += !plain_lit || a != b;
         }
         free(with_restart);
         free(plain);
      }
   }
   for (unsigned pass = 0; pass < 3; ++pass) {
      for (unsigned t = 0; t < 2; ++t) {
       for (unsigned indirect = 0; indirect < 2; ++indirect) {
         unsigned plain_lit, restart_lit;
         GLenum mode = t ? GL_TRIANGLES_ADJACENCY : GL_LINES_ADJACENCY;
         uint64_t plain = render(mode, t ? tris : lines, t ? 12 : 8, 0, indirect, &plain_lit);
         uint64_t restarted = render(mode, t ? tris_restart : lines_restart,
                                     t ? 13 : 9, 1, indirect, &restart_lit);
         printf("pass=%u mode=%s indirect=%u plain=%016" PRIx64 " lit=%u restart=%016" PRIx64
                " lit=%u equal=%u\n", pass, t ? "triangles_adj" : "lines_adj", indirect,
                plain, plain_lit, restarted, restart_lit, plain == restarted);
         failures += !plain_lit || plain != restarted;
       }
      }
   }
   glDeleteFramebuffers(1, &fb);
   glDeleteTextures(1, &tex);
   glDeleteBuffers(1, &ibo);
   glDeleteBuffers(1, &vbo);
   glDeleteVertexArrays(1, &vao);
   glDeleteProgram(program);
   glDeleteShader(vs);
   glDeleteShader(fs);
   eglMakeCurrent(d, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
   eglDestroySurface(d, s);
   eglDestroyContext(d, c);
   eglTerminate(d);
   return failures != 0;
}
