/* SPDX-License-Identifier: GPL-2.0-only */
/* Real ioctl regression probe; run before firmware startup. No STREAMON. */
#include <errno.h>
#include <fcntl.h>
#include <linux/videodev2.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

static void require(int ok, const char *operation)
{
	if (!ok) {
		fprintf(stderr, "V4L2 probe failed: %s (errno=%d)\n", operation, errno);
		exit(1);
	}
}

int main(int argc, char **argv)
{
	const char *path = argc > 1 ? argv[1] : "/dev/video0";
	struct v4l2_capability cap = { 0 };
	int fd = open(path, O_RDWR | O_NONBLOCK);
	int other;

	require(fd >= 0, "open");
	require(!ioctl(fd, VIDIOC_QUERYCAP, &cap), "QUERYCAP");
	require(!strcmp((char *)cap.driver, "apple-ave"), "real AVE driver");
	require((cap.device_caps & (V4L2_CAP_VIDEO_M2M | V4L2_CAP_STREAMING)) ==
		(V4L2_CAP_VIDEO_M2M | V4L2_CAP_STREAMING), "M2M capabilities");
	other = open(path, O_RDWR | O_NONBLOCK);
	require(other < 0 && errno == EBUSY, "exclusive second open");
	for (unsigned int output = 0; output < 2; output++) {
		unsigned int type = output ? V4L2_BUF_TYPE_VIDEO_OUTPUT : V4L2_BUF_TYPE_VIDEO_CAPTURE;
		unsigned int code = output ? V4L2_PIX_FMT_NV12 : V4L2_PIX_FMT_H264;
		unsigned int size = output ? 115200 : 0x40000;
		struct v4l2_format fmt = { .type = type };
		struct v4l2_requestbuffers req = { .type = type, .memory = V4L2_MEMORY_MMAP, .count = 3 };
		struct v4l2_streamparm parm = { .type = type };
		struct v4l2_fmtdesc desc = { .type = type };

		require(!ioctl(fd, VIDIOC_ENUM_FMT, &desc) && desc.pixelformat == code, "ENUM_FMT");
		fmt.fmt.pix.width = 320;
		fmt.fmt.pix.height = 240;
		fmt.fmt.pix.pixelformat = code;
		fmt.fmt.pix.field = V4L2_FIELD_NONE;
		require(!ioctl(fd, VIDIOC_S_FMT, &fmt), "S_FMT with kernel file handle");
		require(fmt.fmt.pix.width == 320 && fmt.fmt.pix.height == 240 &&
			fmt.fmt.pix.pixelformat == code && fmt.fmt.pix.sizeimage == size, "fixed format");
		parm.parm.output.timeperframe.numerator = 1;
		parm.parm.output.timeperframe.denominator = 1;
		require(!ioctl(fd, VIDIOC_S_PARM, &parm), "S_PARM");
		require(!ioctl(fd, VIDIOC_REQBUFS, &req) && req.count >= 3, "REQBUFS");
		for (unsigned int i = 0; i < req.count; i++) {
			struct v4l2_buffer buf = { .type = type, .memory = V4L2_MEMORY_MMAP, .index = i };
			void *memory;

			require(!ioctl(fd, VIDIOC_QUERYBUF, &buf) && buf.length >= size, "QUERYBUF");
			memory = mmap(NULL, buf.length, PROT_READ | PROT_WRITE, MAP_SHARED, fd, buf.m.offset);
			require(memory != MAP_FAILED, "MMAP");
			memset(memory, 0, size);
			require(!munmap(memory, buf.length), "MUNMAP");
		}
		require(ioctl(fd, VIDIOC_S_FMT, &fmt) < 0 && errno == EBUSY, "allocated format locked");
		req.count = 0;
		require(!ioctl(fd, VIDIOC_REQBUFS, &req), "release buffers");
	}
	close(fd);
	fd = open(path, O_RDWR | O_NONBLOCK);
	require(fd >= 0, "reopen after queue release");
	close(fd);
	puts("M5_V4L2_IOCTL_PROBE_PASSED firmware_commands=0 streaming=0");
	return 0;
}
