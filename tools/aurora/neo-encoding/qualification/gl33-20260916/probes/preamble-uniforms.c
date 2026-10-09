/* GL 3.3 uniform publication probe. It performs 64 draws before each readback,
 * without per-draw fences, glFinish, explicit flushes, or memory barriers.
 * A workload name does not prove that the compiler formed a preamble: inspect
 * the candidate's compiler metadata separately before claiming that coverage.
 */
#define _POSIX_C_SOURCE 200809L
#define GL_GLEXT_PROTOTYPES
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GL/gl.h>
#include <GL/glext.h>
#include <inttypes.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

enum { GRID = 8, TILE = 4, WIDTH = GRID * TILE, DRAWS = GRID * GRID, WORDS = 32 };

struct workload {
   const char *name;
   unsigned vertex_words, fragment_words;
   int ubo;
};

static const struct workload workloads[] = {
   {"vs-short-direct", 1, 0, 0}, {"fs-short-direct", 0, 1, 0},
   {"both-short-direct", 1, 1, 0},
   {"vs-long-direct", 32, 0, 0}, {"fs-long-direct", 0, 32, 0},
   {"both-long-direct", 32, 32, 0},
   {"vs-short-ubo", 1, 0, 1}, {"fs-short-ubo", 0, 1, 1},
   {"both-short-ubo", 1, 1, 1},
   {"vs-long-ubo", 32, 0, 1}, {"fs-long-ubo", 0, 32, 1},
   {"both-long-ubo", 32, 32, 1},
};

static void
fail(const char *message)
{
   fprintf(stderr, "%s (GL=0x%x EGL=0x%x)\n", message, glGetError(), eglGetError());
   exit(2);
}

static void
append(char *buffer, size_t size, const char *format, ...)
{
   size_t used = strlen(buffer);
   va_list args;
   va_start(args, format);
   int n = vsnprintf(buffer + used, size - used, format, args);
   va_end(args);
   if (n < 0 || (size_t)n >= size - used)
      fail("shader source overflow");
}

static GLuint
compile_shader(GLenum stage, unsigned words, int ubo)
{
   char source[16384] = "#version 330 core\n";
   const int vertex = stage == GL_VERTEX_SHADER;
   const char *prefix = vertex ? "vs" : "fs";
   if (words) {
      if (ubo)
         append(source, sizeof(source),
                "layout(std140) uniform %s_block { uvec4 %s_data[%u]; };\n",
                prefix, prefix, words);
      else
         append(source, sizeof(source), "uniform uvec4 %s_data[%u];\n",
                prefix, words);
   }
   append(source, sizeof(source), vertex
          ? "flat out uint vertex_hash;\n"
          : "flat in uint vertex_hash;\nlayout(location=0) out uint result;\n");
   append(source, sizeof(source), "void main() {\nuint h = %uu;\n",
          words ? (vertex ? 0x13579bdfu : 0x2468ace0u) : 0u);
   for (unsigned i = 0; i < words; ++i)
      append(source, sizeof(source), "h = (h * 33u) ^ %s_data[%u].x;\n", prefix, i);
   if (vertex)
      append(source, sizeof(source),
             "vertex_hash = h;\n"
             "vec2 p = vec2((gl_VertexID == 1) ? 3.0 : -1.0,\n"
             "              (gl_VertexID == 2) ? 3.0 : -1.0);\n"
             "gl_Position = vec4(p, 0.0, 1.0);\n}\n");
   else
      append(source, sizeof(source),
             "result = vertex_hash ^ h ^ (uint(gl_FragCoord.x) * 65537u)\n"
             "         ^ (uint(gl_FragCoord.y) * 16777217u);\n}\n");

   GLuint shader = glCreateShader(stage);
   const char *text = source;
   glShaderSource(shader, 1, &text, NULL);
   glCompileShader(shader);
   GLint ok;
   glGetShaderiv(shader, GL_COMPILE_STATUS, &ok);
   if (!ok) {
      char log[4096];
      glGetShaderInfoLog(shader, sizeof(log), NULL, log);
      fprintf(stderr, "%s shader: %s\n%s\n", prefix, log, source);
      fail("shader compilation failed");
   }
   return shader;
}

static GLuint
make_program(const struct workload *w)
{
   GLuint vs = compile_shader(GL_VERTEX_SHADER, w->vertex_words, w->ubo);
   GLuint fs = compile_shader(GL_FRAGMENT_SHADER, w->fragment_words, w->ubo);
   GLuint program = glCreateProgram();
   glAttachShader(program, vs);
   glAttachShader(program, fs);
   glLinkProgram(program);
   GLint ok;
   glGetProgramiv(program, GL_LINK_STATUS, &ok);
   if (!ok) {
      char log[4096];
      glGetProgramInfoLog(program, sizeof(log), NULL, log);
      fprintf(stderr, "%s: %s\n", w->name, log);
      fail("program link failed");
   }
   glDeleteShader(vs);
   glDeleteShader(fs);
   return program;
}

/* Only uint32_t operations: CPU overflow matches GLSL unsigned arithmetic.
 * Input words vary independently with round, draw, index and shader stage.
 */
static void
parameters(uint32_t data[WORDS][4], unsigned round, unsigned draw, unsigned stage)
{
   uint32_t state = 0x9e3779b9u ^ (round * 0x85ebca6bu) ^
                    (draw * 0xc2b2ae35u) ^ (stage * 0x27d4eb2du);
   for (unsigned i = 0; i < WORDS; ++i) {
      state ^= state << 13;
      state ^= state >> 17;
      state ^= state << 5;
      data[i][0] = state;
      data[i][1] = data[i][2] = data[i][3] = 0;
   }
}

static uint32_t
reference(const uint32_t data[WORDS][4], unsigned words, unsigned stage)
{
   if (!words)
      return 0;
   uint32_t result = stage == 0 ? 0x13579bdfu : 0x2468ace0u;
   for (unsigned i = 0; i < words; ++i)
      result = result * UINT32_C(33) ^ data[i][0];
   return result;
}

static double
seconds(void)
{
   struct timespec now;
   if (clock_gettime(CLOCK_MONOTONIC, &now))
      fail("clock_gettime failed");
   return now.tv_sec + now.tv_nsec * 1e-9;
}

static int
run(const struct workload *w, unsigned rounds)
{
   GLuint program = make_program(w);
   glUseProgram(program);
   const unsigned counts[2] = {w->vertex_words, w->fragment_words};
   const char *uniforms[2] = {"vs_data[0]", "fs_data[0]"};
   const char *blocks[2] = {"vs_block", "fs_block"};
   GLint locations[2] = {-1, -1};
   GLuint buffers[2] = {0, 0};
   size_t stride[2] = {0, 0};
   unsigned char *upload[2] = {NULL, NULL};
   for (unsigned stage = 0; stage < 2; ++stage) {
      if (!counts[stage])
         continue;
      if (w->ubo) {
         GLuint block = glGetUniformBlockIndex(program, blocks[stage]);
         if (block == GL_INVALID_INDEX)
            fail("uniform block is inactive");
         GLint size, alignment;
         glGetActiveUniformBlockiv(program, block, GL_UNIFORM_BLOCK_DATA_SIZE, &size);
         glGetIntegerv(GL_UNIFORM_BUFFER_OFFSET_ALIGNMENT, &alignment);
         if (size != (GLint)(counts[stage] * 16) || alignment <= 0)
            fail("unexpected std140 block layout");
         stride[stage] = ((size_t)size + alignment - 1) / alignment * alignment;
         upload[stage] = calloc(DRAWS, stride[stage]);
         if (!upload[stage])
            fail("UBO allocation failed");
         glGenBuffers(1, &buffers[stage]);
         glUniformBlockBinding(program, block, stage);
      } else {
         locations[stage] = glGetUniformLocation(program, uniforms[stage]);
         if (locations[stage] < 0)
            fail("uniform array is inactive");
      }
   }

   uint32_t data[2][DRAWS][WORDS][4];
   uint32_t expected[DRAWS], pixels[WIDTH * WIDTH];
   double start = seconds();
   for (unsigned round = 0; round < rounds; ++round) {
      for (unsigned draw = 0; draw < DRAWS; ++draw) {
         for (unsigned stage = 0; stage < 2; ++stage) {
            parameters(data[stage][draw], round, draw, stage);
            if (w->ubo && counts[stage])
               memcpy(upload[stage] + draw * stride[stage], data[stage][draw],
                      counts[stage] * 16);
         }
         expected[draw] = reference(data[0][draw], counts[0], 0) ^
                          reference(data[1][draw], counts[1], 1);
      }
      /* Orphan once per round, then bind distinct aligned ranges for 64 draws.
       * Earlier draws must retain their own inputs until the final readback.
       */
      for (unsigned stage = 0; stage < 2; ++stage) {
         if (w->ubo && counts[stage]) {
            glBindBuffer(GL_UNIFORM_BUFFER, buffers[stage]);
            glBufferData(GL_UNIFORM_BUFFER, DRAWS * stride[stage], upload[stage],
                         GL_STREAM_DRAW);
         }
      }
      const GLuint clear[4] = {0xdeadbeefu, 0, 0, 0};
      glClearBufferuiv(GL_COLOR, 0, clear);
      for (unsigned draw = 0; draw < DRAWS; ++draw) {
         for (unsigned stage = 0; stage < 2; ++stage) {
            if (!counts[stage])
               continue;
            if (w->ubo)
               glBindBufferRange(GL_UNIFORM_BUFFER, stage, buffers[stage],
                                 draw * stride[stage], counts[stage] * 16);
            else
               glUniform4uiv(locations[stage], counts[stage], &data[stage][draw][0][0]);
         }
         glViewport((draw % GRID) * TILE, (draw / GRID) * TILE, TILE, TILE);
         glDrawArrays(GL_TRIANGLES, 0, 3);
      }
      glReadPixels(0, 0, WIDTH, WIDTH, GL_RED_INTEGER, GL_UNSIGNED_INT, pixels);
      if (glGetError() != GL_NO_ERROR)
         fail("GL error in uniform draw/readback round");
      for (unsigned y = 0; y < WIDTH; ++y) {
         for (unsigned x = 0; x < WIDTH; ++x) {
            unsigned draw = (y / TILE) * GRID + x / TILE;
            uint32_t value = expected[draw] ^ (x * UINT32_C(65537)) ^
                             (y * UINT32_C(16777217));
            if (pixels[y * WIDTH + x] != value) {
               fprintf(stderr, "FAIL %s round=%u draw=%u pixel=%u,%u expected=%08"
                       PRIx32 " actual=%08" PRIx32 "\n", w->name, round, draw,
                       x, y, value, pixels[y * WIDTH + x]);
               return 1;
            }
         }
      }
   }
   printf("PASS %s rounds=%u draws=%u pixels=%u elapsed_ms=%.3f\n",
          w->name, rounds, rounds * DRAWS, rounds * WIDTH * WIDTH,
          (seconds() - start) * 1000);
   fflush(stdout);
   glDeleteBuffers(2, buffers);
   free(upload[0]);
   free(upload[1]);
   glDeleteProgram(program);
   return 0;
}

int
main(int argc, char **argv)
{
   if (argc < 2 || argc > 4) {
      fprintf(stderr, "usage: %s expected-renderer-substring [case|all] [rounds]\n", argv[0]);
      return 2;
   }
   const char *selected = argc >= 3 ? argv[2] : "all";
   char *end = NULL;
   unsigned long parsed = argc == 4 ? strtoul(argv[3], &end, 10) : 8;
   if (!parsed || parsed > 10000 || (end && *end))
      fail("rounds must be 1..10000");
   unsigned rounds = parsed;
   int found = strcmp(selected, "all") == 0;
   for (unsigned i = 0; i < sizeof(workloads) / sizeof(workloads[0]); ++i)
      found |= strcmp(selected, workloads[i].name) == 0;
   if (!found)
      fail("unknown workload");

   EGLDisplay display = eglGetDisplay(EGL_DEFAULT_DISPLAY);
   EGLint major, minor;
   if (display == EGL_NO_DISPLAY || !eglInitialize(display, &major, &minor) ||
       !eglBindAPI(EGL_OPENGL_API))
      fail("EGL initialization failed");
   const EGLint attributes[] = {EGL_SURFACE_TYPE, EGL_PBUFFER_BIT,
       EGL_RENDERABLE_TYPE, EGL_OPENGL_BIT, EGL_NONE};
   EGLConfig config;
   EGLint count;
   if (!eglChooseConfig(display, attributes, &config, 1, &count) || count != 1)
      fail("EGL configuration failed");
   const EGLint context_attributes[] = {EGL_CONTEXT_MAJOR_VERSION, 3,
       EGL_CONTEXT_MINOR_VERSION, 3, EGL_CONTEXT_OPENGL_PROFILE_MASK,
       EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT, EGL_NONE};
   const EGLint surface_attributes[] = {EGL_WIDTH, WIDTH, EGL_HEIGHT, WIDTH, EGL_NONE};
   EGLContext context = eglCreateContext(display, config, EGL_NO_CONTEXT, context_attributes);
   EGLSurface surface = eglCreatePbufferSurface(display, config, surface_attributes);
   if (context == EGL_NO_CONTEXT || surface == EGL_NO_SURFACE ||
       !eglMakeCurrent(display, surface, surface, context))
      fail("EGL context failed");
   const char *renderer = (const char *)glGetString(GL_RENDERER);
   if (!renderer || !strstr(renderer, argv[1]))
      fail("unexpected renderer");
   printf("renderer=%s version=%s\n", renderer, (const char *)glGetString(GL_VERSION));
   GLint gl_major, gl_minor, profile;
   glGetIntegerv(GL_MAJOR_VERSION, &gl_major);
   glGetIntegerv(GL_MINOR_VERSION, &gl_minor);
   glGetIntegerv(GL_CONTEXT_PROFILE_MASK, &profile);
   if (gl_major < 3 || (gl_major == 3 && gl_minor < 3) ||
       !(profile & GL_CONTEXT_CORE_PROFILE_BIT))
      fail("OpenGL 3.3 core required");

   GLuint vao, fbo, texture;
   glGenVertexArrays(1, &vao);
   glBindVertexArray(vao);
   glGenTextures(1, &texture);
   glBindTexture(GL_TEXTURE_2D, texture);
   glTexImage2D(GL_TEXTURE_2D, 0, GL_R32UI, WIDTH, WIDTH, 0,
                GL_RED_INTEGER, GL_UNSIGNED_INT, NULL);
   glGenFramebuffers(1, &fbo);
   glBindFramebuffer(GL_FRAMEBUFFER, fbo);
   glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D,
                          texture, 0);
   glDrawBuffer(GL_COLOR_ATTACHMENT0);
   glReadBuffer(GL_COLOR_ATTACHMENT0);
   if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE)
      fail("integer framebuffer incomplete");
   if (glGetError() != GL_NO_ERROR)
      fail("GL setup error");
   unsigned executed = 0;
   for (unsigned i = 0; i < sizeof(workloads) / sizeof(workloads[0]); ++i) {
      if (strcmp(selected, "all") && strcmp(selected, workloads[i].name))
         continue;
      if (run(&workloads[i], rounds))
         return 1;
      ++executed;
   }
   glDeleteFramebuffers(1, &fbo);
   glDeleteTextures(1, &texture);
   glDeleteVertexArrays(1, &vao);
   eglMakeCurrent(display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
   eglDestroySurface(display, surface);
   eglDestroyContext(display, context);
   eglTerminate(display);
   printf("PASS workloads=%u draws=%u pixel_comparisons=%u\n",
          executed, executed * rounds * DRAWS, executed * rounds * WIDTH * WIDTH);
   return 0;
}
