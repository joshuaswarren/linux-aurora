/* SPDX-License-Identifier: MIT */
/* Reproduce render timestamp publication ordering through Zink. */
#define GL_GLEXT_PROTOTYPES
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GL/glcorearb.h>
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>

static GLuint shader(GLenum stage, const char *source)
{
  GLuint s = glCreateShader(stage);
  glShaderSource(s, 1, &source, NULL);
  glCompileShader(s);
  GLint ok;
  glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
  if (!ok) exit(2);
  return s;
}

int main(void)
{
  EGLDisplay d = eglGetPlatformDisplay(EGL_PLATFORM_SURFACELESS_MESA,
                                      EGL_DEFAULT_DISPLAY, NULL);
  if (!eglInitialize(d, NULL, NULL) || !eglBindAPI(EGL_OPENGL_API)) return 2;
  const EGLint attrs[] = {
    EGL_CONTEXT_MAJOR_VERSION, 4, EGL_CONTEXT_MINOR_VERSION, 5,
    EGL_CONTEXT_OPENGL_PROFILE_MASK, EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT,
    EGL_NONE,
  };
  EGLContext c = eglCreateContext(d, NULL, EGL_NO_CONTEXT, attrs);
  if (c == EGL_NO_CONTEXT || !eglMakeCurrent(d, EGL_NO_SURFACE,
                                             EGL_NO_SURFACE, c)) return 3;
  fprintf(stderr, "renderer: %s\n", glGetString(GL_RENDERER));
  GLuint tex, fb, vao;
  glGenTextures(1, &tex);
  glBindTexture(GL_TEXTURE_2D, tex);
  glTexStorage2D(GL_TEXTURE_2D, 1, GL_RGBA8, 64, 64);
  glGenFramebuffers(1, &fb);
  glBindFramebuffer(GL_FRAMEBUFFER, fb);
  glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0,
                         GL_TEXTURE_2D, tex, 0);
  if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE)
    return 4;
  glGenVertexArrays(1, &vao);
  glBindVertexArray(vao);
  GLuint p = glCreateProgram();
  glAttachShader(p, shader(GL_VERTEX_SHADER,
    "#version 450\nvoid main(){vec2 p[3]=vec2[](vec2(-1,-1),vec2(3,-1),vec2(-1,3));gl_Position=vec4(p[gl_VertexID],0,1);}"));
  glAttachShader(p, shader(GL_FRAGMENT_SHADER,
    "#version 450\nlayout(location=0)out vec4 c;void main(){c=vec4(0.25,0.5,0.75,1);}"));
  glLinkProgram(p);
  GLint ok;
  glGetProgramiv(p, GL_LINK_STATUS, &ok);
  if (!ok) return 5;
  glUseProgram(p);
  glViewport(0, 0, 64, 64);
  GLuint q[14];
  glGenQueries(14, q);
  for (unsigned frame = 0; frame < 32; frame++) {
    for (unsigned i = 0; i < 14; i++) {
      glBeginQuery(GL_TIME_ELAPSED, q[i]);
      if (i % 4 != 1) glDrawArrays(GL_TRIANGLES, 0, 3);
      glEndQuery(GL_TIME_ELAPSED);
    }
    glFlush();
    glFinish();
    GLuint64 total = 0;
    for (unsigned i = 0; i < 14; i++) {
      GLuint64 ns;
      glGetQueryObjectui64v(q[i], GL_QUERY_RESULT, &ns);
      if (ns > 1000000000ULL) {
        fprintf(stderr, "invalid elapsed timestamp: frame=%u query=%u ns=%" PRIu64 "\n", frame, i, ns);
        return 6;
      }
      total += ns;
    }
    if (glGetError() != GL_NO_ERROR) return 7;
    printf("frame=%u elapsed=%" PRIu64 "ns\n", frame, total);
    fflush(stdout);
  }
  puts("PASS: 448 elapsed-time queries, including empty queries");
  return 0;
}
