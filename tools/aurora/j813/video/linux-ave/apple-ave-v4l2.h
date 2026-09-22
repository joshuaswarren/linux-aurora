/* SPDX-License-Identifier: GPL-2.0-only */
/* Fixed-profile native AVE encoder. Included after the IPC transport. */
#include <linux/firmware.h>
#include <linux/workqueue.h>
#include <media/v4l2-device.h>
#include <media/v4l2-event.h>
#include <media/v4l2-ioctl.h>
#include <media/v4l2-mem2mem.h>
#include <media/videobuf2-vmalloc.h>

#define AVE_WIDTH 320
#define AVE_HEIGHT 240
#define AVE_RAW_BYTES 115200
#define AVE_CAPTURE_BYTES 0x40000
#define AVE_SURFACE_BYTES 0x528000
#define AVE_INPUT_OFF 0x600200
#define AVE_START_BYTES 0x3180
#define AVE_PROCESS_BYTES 0x63d8

struct ave_video {
	struct apple_ave *ave;
	struct v4l2_device v4l2;
	struct video_device vdev;
	struct v4l2_m2m_dev *m2m;
	struct mutex lock;
	u8 *start, *process;
	u32 client;
	u64 frames, encode_ns;
	bool opened;
};

struct ave_video_ctx {
	struct v4l2_fh fh;
	struct ave_video *video;
	struct work_struct work;
	spinlock_t state_lock;
	u32 sequence;
};

static const struct v4l2_event ave_eos = { .type = V4L2_EVENT_EOS };

static void ave_video_status(struct apple_ave *ave, struct seq_file *s)
{
	if (ave->video)
		seq_printf(s, "v4l2_frames=%llu\nv4l2_encode_ns=%llu\n",
			   ave->video->frames, ave->video->encode_ns);
}

static int ave_encoder_reply(struct ave_video *v, u8 *packet, size_t size, u32 op)
{
	struct apple_ave *ave = v->ave;
	u32 expected[20] = { 0 };
	size_t bytes;
	int ret;

	put_unaligned_le64(v->client, packet + 0x10);
	ret = ave_submit_locked(ave, packet, size);
	if (ret)
		return ret;
	bytes = get_unaligned_le32(ave->response + 4);
	expected[0] = cpu_to_le32(op);
	expected[4] = cpu_to_le32(v->client);
	expected[8] = cpu_to_le32(op == 8 ? 0x15 : op);
	if (ave->response_bytes != bytes + 24 || (bytes != 72 && bytes != 80) ||
	    get_unaligned_le32(ave->response) != 2 ||
	    memcmp(ave->response + 24, expected, bytes)) {
		ave->command_failed = true;
		return -EPROTO;
	}
	return 0;
}

static int ave_encoder_simple(struct ave_video *v, u32 op, u32 request)
{
	u8 packet[72] = { 0 };

	put_unaligned_le32(op, packet);
	put_unaligned_le64(request, packet + 8);
	put_unaligned_le32(1, packet + 0x18);
	put_unaligned_le32(1, packet + 0x1c);
	put_unaligned_le32(op, packet + 0x20);
	put_unaligned_le32(op == 9 ? 0 : 0xc8, packet + 0x24);
	put_unaligned_le32(0x36, packet + 0x28);
	put_unaligned_le32(op == 3 ? 0x1e0 : 0x140, packet + 0x2c);
	return ave_encoder_reply(v, packet, sizeof(packet), op);
}

/* Parameter sets are copied from firmware metadata, never synthesized. */
static int ave_encoder_output(struct ave_video *v, u8 *output)
{
	u8 *arena = v->ave->ipc.cpu + AVE_ARENA_OFF;
	u8 *meta = arena + 0x1f4000, *stream = arena + 0xb4000;
	u32 bytes = get_unaligned_le32(meta + 0x120);
	u32 positions[2] = { 0 }, lengths[2] = { 0 };
	u32 i, end, kind, total = 0;

	if (bytes < 5 || bytes > 0x3c000 ||
	    bytes != get_unaligned_le32(meta + 0x12c) ||
	    get_unaligned_be32(stream) != 1 || (stream[4] & 31) != 5)
		return -EPROTO;
	for (i = 0; i + 5 < 0xc000; i++) {
		if (get_unaligned_be32(meta + i) != 1 || (meta[i + 4] & 0x80))
			continue;
		kind = meta[i + 4] & 31;
		if (kind != 7 && kind != 8)
			continue;
		if (lengths[kind - 7])
			return -EPROTO;
		for (end = i + 5; end + 4 <= 0xc000; end++)
			if (get_unaligned_be32(meta + end) == 1)
				break;
		if (end + 4 > 0xc000)
			end = 0xc000;
		while (end > i + 5 && !meta[end - 1])
			end--;
		if (end - i > 4096)
			return -EPROTO;
		positions[kind - 7] = i;
		lengths[kind - 7] = end - i;
	}
	for (i = 0; i < 2; i++) {
		if (!lengths[i])
			return -EPROTO;
		memcpy(output + total, meta + positions[i], lengths[i]);
		total += lengths[i];
	}
	if (total + bytes > AVE_CAPTURE_BYTES)
		return -EOVERFLOW;
	memcpy(output + total, stream, bytes);
	return total + bytes;
}

static int ave_encode_frame(struct ave_video *v, const u8 *input, u8 *output)
{
	struct apple_ave *ave = v->ave;
	u8 *arena = ave->ipc.cpu + AVE_ARENA_OFF;
	u64 started = ktime_get_ns();
	int ret;

	mutex_lock(&ave->lock);
	if (!ave->encoder_owned || ave->command_failed || v->client > 0xffff) {
		ret = -EIO;
		goto out;
	}
	memset(arena, 0, AVE_SURFACE_BYTES);
	dma_wmb();
	ret = ave_encoder_simple(v, 3, 1);
	if (ret)
		goto out;
	put_unaligned_le64(2, v->start + 8);
	ret = ave_encoder_reply(v, v->start, AVE_START_BYTES, 6);
	if (ret)
		goto out;
	memcpy(arena + AVE_INPUT_OFF, input, AVE_RAW_BYTES);
	dma_wmb();
	put_unaligned_le64(3, v->process + 8);
	ret = ave_encoder_reply(v, v->process, AVE_PROCESS_BYTES, 8);
	if (ret)
		goto out;
	ret = ave_encoder_simple(v, 9, 4);
	if (ret)
		goto out;
	ret = ave_encoder_simple(v, 7, 5);
	if (ret)
		goto out;
	ret = ave_encoder_simple(v, 4, 6);
	if (ret)
		goto out;
	dma_rmb();
	ret = ave_encoder_output(v, output);
	if (ret < 0) {
		ave->command_failed = true;
		goto out;
	}
	v->client++;
	v->frames++;
	v->encode_ns += ktime_get_ns() - started;
out:
	mutex_unlock(&ave->lock);
	return ret;
}

static void ave_video_work(struct work_struct *work)
{
	struct ave_video_ctx *ctx = container_of(work, struct ave_video_ctx, work);
	struct v4l2_m2m_ctx *m = ctx->fh.m2m_ctx;
	struct vb2_v4l2_buffer *src = v4l2_m2m_next_src_buf(m);
	struct vb2_v4l2_buffer *dst = v4l2_m2m_dst_buf_remove(m);
	int bytes = ave_encode_frame(ctx->video, vb2_plane_vaddr(&src->vb2_buf, 0),
				     vb2_plane_vaddr(&dst->vb2_buf, 0));
	enum vb2_buffer_state state = bytes < 0 ? VB2_BUF_STATE_ERROR : VB2_BUF_STATE_DONE;

	v4l2_m2m_buf_copy_metadata(src, dst);
	dst->flags &= ~(V4L2_BUF_FLAG_PFRAME | V4L2_BUF_FLAG_BFRAME | V4L2_BUF_FLAG_LAST);
	dst->flags |= V4L2_BUF_FLAG_KEYFRAME;
	dst->field = V4L2_FIELD_NONE;
	src->sequence = dst->sequence = ctx->sequence++;
	vb2_set_plane_payload(&dst->vb2_buf, 0, bytes < 0 ? 0 : bytes);
	spin_lock(&ctx->state_lock);
	if (v4l2_m2m_is_last_draining_src_buf(m, src)) {
		dst->flags |= V4L2_BUF_FLAG_LAST;
		v4l2_m2m_mark_stopped(m);
		v4l2_event_queue_fh(&ctx->fh, &ave_eos);
	}
	v4l2_m2m_src_buf_remove(m);
	v4l2_m2m_buf_done(src, state);
	v4l2_m2m_buf_done(dst, state);
	spin_unlock(&ctx->state_lock);
	if (bytes < 0) {
		vb2_queue_error(&m->out_q_ctx.q);
		vb2_queue_error(&m->cap_q_ctx.q);
	}
	v4l2_m2m_job_finish(ctx->video->m2m, m);
}

static void ave_video_run(void *priv)
{
	struct ave_video_ctx *ctx = priv;

	schedule_work(&ctx->work);
}

static void ave_video_abort(void *priv)
{
	/* The bounded in-flight command must finish before buffers are returned.
	 * m2m waits for ave_video_work to call job_finish; never free DMA early. */
}

static int ave_queue_setup(struct vb2_queue *q, unsigned int *count,
			   unsigned int *planes, unsigned int sizes[], struct device *devs[])
{
	u32 size = V4L2_TYPE_IS_OUTPUT(q->type) ? AVE_RAW_BYTES : AVE_CAPTURE_BYTES;

	if (*planes)
		return *planes == 1 && sizes[0] >= size ? 0 : -EINVAL;
	*planes = 1;
	sizes[0] = size;
	return 0;
}

static int ave_buf_prepare(struct vb2_buffer *buf)
{
	bool output = V4L2_TYPE_IS_OUTPUT(buf->vb2_queue->type);
	u32 size = output ? AVE_RAW_BYTES : AVE_CAPTURE_BYTES;

	if (vb2_plane_size(buf, 0) < size ||
	    (output && vb2_get_plane_payload(buf, 0) != AVE_RAW_BYTES))
		return -EINVAL;
	return 0;
}

static void ave_buf_queue(struct vb2_buffer *buf)
{
	struct ave_video_ctx *ctx = vb2_get_drv_priv(buf->vb2_queue);
	struct vb2_v4l2_buffer *vb = to_vb2_v4l2_buffer(buf);

	spin_lock(&ctx->state_lock);
	if (V4L2_TYPE_IS_CAPTURE(buf->vb2_queue->type) &&
	    vb2_is_streaming(buf->vb2_queue) && v4l2_m2m_dst_buf_is_last(ctx->fh.m2m_ctx)) {
		vb2_set_plane_payload(buf, 0, 0);
		vb->sequence = ctx->sequence;
		vb->field = V4L2_FIELD_NONE;
		v4l2_m2m_last_buffer_done(ctx->fh.m2m_ctx, vb);
		v4l2_event_queue_fh(&ctx->fh, &ave_eos);
	} else {
		v4l2_m2m_buf_queue(ctx->fh.m2m_ctx, vb);
	}
	spin_unlock(&ctx->state_lock);
}

static void ave_stop_streaming(struct vb2_queue *q)
{
	struct ave_video_ctx *ctx = vb2_get_drv_priv(q);
	struct vb2_v4l2_buffer *buf;

	cancel_work_sync(&ctx->work);
	while ((buf = V4L2_TYPE_IS_OUTPUT(q->type) ?
		v4l2_m2m_src_buf_remove(ctx->fh.m2m_ctx) : v4l2_m2m_dst_buf_remove(ctx->fh.m2m_ctx)))
		v4l2_m2m_buf_done(buf, VB2_BUF_STATE_ERROR);
	v4l2_m2m_update_stop_streaming_state(ctx->fh.m2m_ctx, q);
}

static const struct vb2_ops ave_queue_ops = {
	.queue_setup = ave_queue_setup,
	.buf_prepare = ave_buf_prepare,
	.buf_queue = ave_buf_queue,
	.stop_streaming = ave_stop_streaming,
};

static int ave_queue_init(void *priv, struct vb2_queue *src, struct vb2_queue *dst)
{
	struct ave_video_ctx *ctx = priv;
	struct vb2_queue *queues[] = { src, dst };
	int ret;

	for (unsigned int i = 0; i < 2; i++) {
		struct vb2_queue *q = queues[i];

		q->type = i ? V4L2_BUF_TYPE_VIDEO_CAPTURE : V4L2_BUF_TYPE_VIDEO_OUTPUT;
		q->io_modes = VB2_MMAP;
		q->drv_priv = ctx;
		q->buf_struct_size = sizeof(struct v4l2_m2m_buffer);
		q->ops = &ave_queue_ops;
		q->mem_ops = &vb2_vmalloc_memops;
		q->timestamp_flags = V4L2_BUF_FLAG_TIMESTAMP_COPY;
		q->lock = &ctx->video->lock;
		q->max_num_buffers = 32;
		q->min_reqbufs_allocation = 2;
		ret = vb2_queue_init(q);
		if (ret)
			return ret;
	}
	return 0;
}

static int ave_querycap(struct file *file, void *priv, struct v4l2_capability *cap)
{
	strscpy(cap->driver, "apple-ave", sizeof(cap->driver));
	strscpy(cap->card, "Apple M5 AVE H.264", sizeof(cap->card));
	strscpy(cap->bus_info, "platform:281100000.video-codec", sizeof(cap->bus_info));
	return 0;
}

static int ave_enum_fmt(struct file *file, void *priv, struct v4l2_fmtdesc *fmt)
{
	if (fmt->index)
		return -EINVAL;
	fmt->pixelformat = V4L2_TYPE_IS_OUTPUT(fmt->type) ? V4L2_PIX_FMT_NV12 : V4L2_PIX_FMT_H264;
	return 0;
}

static int ave_get_fmt(struct file *file, void *priv, struct v4l2_format *fmt)
{
	bool output = V4L2_TYPE_IS_OUTPUT(fmt->type);
	struct v4l2_pix_format *pix = &fmt->fmt.pix;

	memset(pix, 0, sizeof(*pix));
	pix->width = AVE_WIDTH;
	pix->height = AVE_HEIGHT;
	pix->pixelformat = output ? V4L2_PIX_FMT_NV12 : V4L2_PIX_FMT_H264;
	pix->field = V4L2_FIELD_NONE;
	pix->bytesperline = output ? AVE_WIDTH : 0;
	pix->sizeimage = output ? AVE_RAW_BYTES : AVE_CAPTURE_BYTES;
	pix->colorspace = V4L2_COLORSPACE_REC709;
	pix->ycbcr_enc = V4L2_YCBCR_ENC_709;
	pix->quantization = V4L2_QUANTIZATION_LIM_RANGE;
	pix->xfer_func = V4L2_XFER_FUNC_709;
	return 0;
}

static int ave_set_fmt(struct file *file, void *priv, struct v4l2_format *fmt)
{
	struct ave_video_ctx *ctx = container_of(file_to_v4l2_fh(file), struct ave_video_ctx, fh);

	if (vb2_is_busy(v4l2_m2m_get_vq(ctx->fh.m2m_ctx, fmt->type)))
		return -EBUSY;
	return ave_get_fmt(file, priv, fmt);
}

static int ave_enum_size(struct file *file, void *priv, struct v4l2_frmsizeenum *size)
{
	if (size->index || (size->pixel_format != V4L2_PIX_FMT_NV12 &&
			   size->pixel_format != V4L2_PIX_FMT_H264))
		return -EINVAL;
	size->type = V4L2_FRMSIZE_TYPE_DISCRETE;
	size->discrete.width = AVE_WIDTH;
	size->discrete.height = AVE_HEIGHT;
	return 0;
}

static int ave_enum_interval(struct file *file, void *priv, struct v4l2_frmivalenum *iv)
{
	if (iv->index || iv->width != AVE_WIDTH || iv->height != AVE_HEIGHT ||
	    (iv->pixel_format != V4L2_PIX_FMT_NV12 && iv->pixel_format != V4L2_PIX_FMT_H264))
		return -EINVAL;
	iv->type = V4L2_FRMIVAL_TYPE_DISCRETE;
	iv->discrete.numerator = iv->discrete.denominator = 1;
	return 0;
}

static int ave_streamparm(struct file *file, void *priv, struct v4l2_streamparm *parm)
{
	if (parm->type != V4L2_BUF_TYPE_VIDEO_OUTPUT && parm->type != V4L2_BUF_TYPE_VIDEO_CAPTURE)
		return -EINVAL;
	memset(&parm->parm, 0, sizeof(parm->parm));
	parm->parm.output.capability = V4L2_CAP_TIMEPERFRAME;
	parm->parm.output.timeperframe.numerator = 1;
	parm->parm.output.timeperframe.denominator = 1;
	return 0;
}

static int ave_encoder_cmd(struct file *file, void *priv, struct v4l2_encoder_cmd *cmd)
{
	struct ave_video_ctx *ctx = container_of(file_to_v4l2_fh(file), struct ave_video_ctx, fh);
	int ret = v4l2_m2m_ioctl_try_encoder_cmd(file, priv, cmd);

	if (ret || !vb2_is_streaming(&ctx->fh.m2m_ctx->out_q_ctx.q))
		return ret;
	spin_lock(&ctx->state_lock);
	ret = v4l2_m2m_ioctl_encoder_cmd(file, priv, cmd);
	if (!ret && cmd->cmd == V4L2_ENC_CMD_STOP && v4l2_m2m_has_stopped(ctx->fh.m2m_ctx))
		v4l2_event_queue_fh(&ctx->fh, &ave_eos);
	spin_unlock(&ctx->state_lock);
	return ret;
}

static int ave_subscribe(struct v4l2_fh *fh, const struct v4l2_event_subscription *sub)
{
	return sub->type == V4L2_EVENT_EOS ? v4l2_event_subscribe(fh, sub, 2, NULL) : -EINVAL;
}

static const struct v4l2_ioctl_ops ave_ioctl_ops = {
	.vidioc_querycap = ave_querycap,
	.vidioc_enum_fmt_vid_cap = ave_enum_fmt,
	.vidioc_enum_fmt_vid_out = ave_enum_fmt,
	.vidioc_g_fmt_vid_cap = ave_get_fmt,
	.vidioc_g_fmt_vid_out = ave_get_fmt,
	.vidioc_try_fmt_vid_cap = ave_get_fmt,
	.vidioc_try_fmt_vid_out = ave_get_fmt,
	.vidioc_s_fmt_vid_cap = ave_set_fmt,
	.vidioc_s_fmt_vid_out = ave_set_fmt,
	.vidioc_enum_framesizes = ave_enum_size,
	.vidioc_enum_frameintervals = ave_enum_interval,
	.vidioc_g_parm = ave_streamparm,
	.vidioc_s_parm = ave_streamparm,
	.vidioc_reqbufs = v4l2_m2m_ioctl_reqbufs,
	.vidioc_querybuf = v4l2_m2m_ioctl_querybuf,
	.vidioc_qbuf = v4l2_m2m_ioctl_qbuf,
	.vidioc_dqbuf = v4l2_m2m_ioctl_dqbuf,
	.vidioc_create_bufs = v4l2_m2m_ioctl_create_bufs,
	.vidioc_prepare_buf = v4l2_m2m_ioctl_prepare_buf,
	.vidioc_streamon = v4l2_m2m_ioctl_streamon,
	.vidioc_streamoff = v4l2_m2m_ioctl_streamoff,
	.vidioc_try_encoder_cmd = v4l2_m2m_ioctl_try_encoder_cmd,
	.vidioc_encoder_cmd = ave_encoder_cmd,
	.vidioc_subscribe_event = ave_subscribe,
	.vidioc_unsubscribe_event = v4l2_event_unsubscribe,
};

static int ave_video_open(struct file *file)
{
	struct ave_video *v = video_drvdata(file);
	struct ave_video_ctx *ctx;
	int ret = 0;

	mutex_lock(&v->lock);
	if (v->opened) {
		ret = -EBUSY;
		goto out;
	}
	ctx = kzalloc(sizeof(*ctx), GFP_KERNEL);
	if (!ctx) {
		ret = -ENOMEM;
		goto out;
	}
	ctx->video = v;
	spin_lock_init(&ctx->state_lock);
	INIT_WORK(&ctx->work, ave_video_work);
	v4l2_fh_init(&ctx->fh, &v->vdev);
	file->private_data = &ctx->fh;
	ctx->fh.m2m_ctx = v4l2_m2m_ctx_init(v->m2m, ctx, ave_queue_init);
	if (IS_ERR(ctx->fh.m2m_ctx)) {
		ret = PTR_ERR(ctx->fh.m2m_ctx);
		v4l2_fh_exit(&ctx->fh);
		kfree(ctx);
		goto out;
	}
	v4l2_fh_add(&ctx->fh, file);
	v->opened = true;
out:
	mutex_unlock(&v->lock);
	return ret;
}

static int ave_video_release(struct file *file)
{
	struct ave_video_ctx *ctx = container_of(file_to_v4l2_fh(file), struct ave_video_ctx, fh);
	struct ave_video *v = ctx->video;

	mutex_lock(&v->lock);
	v4l2_m2m_ctx_release(ctx->fh.m2m_ctx);
	cancel_work_sync(&ctx->work);
	v4l2_fh_del(&ctx->fh, file);
	v4l2_fh_exit(&ctx->fh);
	v->opened = false;
	mutex_unlock(&v->lock);
	kfree(ctx);
	return 0;
}

static const struct v4l2_file_operations ave_video_fops = {
	.owner = THIS_MODULE,
	.open = ave_video_open,
	.release = ave_video_release,
	.poll = v4l2_m2m_fop_poll,
	.mmap = v4l2_m2m_fop_mmap,
	.unlocked_ioctl = video_ioctl2,
};

static const struct v4l2_m2m_ops ave_m2m_ops = {
	.device_run = ave_video_run,
	.job_abort = ave_video_abort,
};

/* Privileged setup for the captured fixed profile. Templates contain this
 * boot's relocated IOVAs and must be regenerated after every reboot. */
static ssize_t ave_video_enable(struct file *file, const char __user *data,
			       size_t count, loff_t *pos)
{
	static const u32 offsets[] = {
		0x48, 0x390, 0x3a0, 0x3b0, 0x3c0, 0x3d0, 0x3e0, 0x3f0, 0x400, 0x410, 0x420,
		0x7d0, 0x7e0, 0x7f0, 0x800, 0x810, 0x820, 0x830, 0x840, 0x850, 0x860,
		0xc10, 0xd50, 0xd60, 0xd70, 0xd80, 0xd90, 0xf50, 0xf60, 0xf70, 0xf80, 0xf90,
		0x1130, 0x1140, 0x1150, 0x1160, 0x1170, 0x1350, 0x1390, 0x13d0,
		0x1410, 0x1420, 0x1430, 0x1440, 0x1490, 0x14a0, 0x14b0, 0x14c0,
		0x1510, 0x1520, 0x1530, 0x1540, 0x1590, 0x15a0, 0x15b0, 0x15c0,
	};
	struct apple_ave *ave = file->private_data;
	struct ave_video *v = ave->video;
	const struct firmware *start = NULL, *process = NULL;
	u64 base = ave->ipc.dma + AVE_ARENA_OFF;
	bool requested;
	int ret;

	if (!capable(CAP_SYS_RAWIO))
		return -EPERM;
	ret = kstrtobool_from_user(data, count, &requested);
	if (ret || !requested)
		return ret ?: -EINVAL;
	mutex_lock(&v->lock);
	mutex_lock(&ave->lock);
	if (v->opened || ave->encoder_owned || !ave->ipc_ready || ave->command_failed ||
	    ave->response_bytes < 96 ||
	    (get_unaligned_le32(ave->response + 24) != 1 &&
	     get_unaligned_le32(ave->response + 24) != 4)) {
		ret = -EBUSY;
		goto out;
	}
	ret = request_firmware_direct(&start, "apple/ave/m5-start.bin", ave->dev);
	if (ret)
		goto out;
	ret = request_firmware_direct(&process, "apple/ave/m5-process.bin", ave->dev);
	if (ret)
		goto out;
	ret = -EINVAL;
	if (start->size != AVE_START_BYTES || process->size != AVE_PROCESS_BYTES ||
	    get_unaligned_le32(start->data) != 6 || get_unaligned_le32(process->data) != 8 ||
	    ave->ipc.size - AVE_ARENA_OFF < AVE_INPUT_OFF + AVE_RAW_BYTES)
		goto out;
	for (unsigned int i = 0; i < ARRAY_SIZE(offsets); i++) {
		u64 delta = get_unaligned_le64(start->data + offsets[i]) - base;
		u32 bytes = get_unaligned_le32(start->data + offsets[i] + 8);

		if (delta >= AVE_SURFACE_BYTES || bytes > AVE_SURFACE_BYTES - delta)
			goto out;
	}
	if (get_unaligned_le64(process->data + 0x5830) != base + AVE_INPUT_OFF ||
	    get_unaligned_le64(process->data + 0x5850) != base + AVE_INPUT_OFF + 76800 ||
	    get_unaligned_le32(process->data + 0x5838) != 76800 ||
	    get_unaligned_le32(process->data + 0x5858) != 38400 ||
	    get_unaligned_le32(process->data + 0x583c) != AVE_WIDTH ||
	    get_unaligned_le32(process->data + 0x585c) != AVE_WIDTH ||
	    get_unaligned_le32(process->data + 0x12c0) != 1 ||
	    get_unaligned_le32(process->data + 0x6228) ||
	    get_unaligned_le64(process->data + 0x6270))
		goto out;
	for (unsigned int off = 0x61b8; off <= 0x61c0; off += 8)
		if (get_unaligned_le64(process->data + off) - base >= AVE_SURFACE_BYTES)
			goto out;
	v->start = kmemdup(start->data, start->size, GFP_KERNEL);
	v->process = kmemdup(process->data, process->size, GFP_KERNEL);
	if (!v->start || !v->process) {
		kfree(v->start);
		kfree(v->process);
		v->start = v->process = NULL;
		ret = -ENOMEM;
		goto out;
	}
	v->client = get_unaligned_le64(ave->response + 24 + 0x10) + 1;
	ave->encoder_owned = true;
	ret = 0;
	dev_info(ave->dev, "AVE_V4L2_READY 320x240 NV12 H.264 1fps all-IDR\n");
out:
	release_firmware(process);
	release_firmware(start);
	mutex_unlock(&ave->lock);
	mutex_unlock(&v->lock);
	return ret ?: count;
}

static const struct file_operations ave_video_enable_fops = {
	.owner = THIS_MODULE,
	.open = simple_open,
	.write = ave_video_enable,
};

static int ave_video_register(struct apple_ave *ave)
{
	struct ave_video *v = kzalloc(sizeof(*v), GFP_KERNEL);
	int ret;

	if (!v)
		return -ENOMEM;
	v->ave = ave;
	mutex_init(&v->lock);
	ret = v4l2_device_register(ave->dev, &v->v4l2);
	if (ret)
		goto free;
	v->m2m = v4l2_m2m_init(&ave_m2m_ops);
	if (IS_ERR(v->m2m)) {
		ret = PTR_ERR(v->m2m);
		goto unregister;
	}
	strscpy(v->vdev.name, "Apple M5 AVE H.264", sizeof(v->vdev.name));
	v->vdev.v4l2_dev = &v->v4l2;
	v->vdev.fops = &ave_video_fops;
	v->vdev.ioctl_ops = &ave_ioctl_ops;
	v->vdev.release = video_device_release_empty;
	v->vdev.lock = &v->lock;
	v->vdev.vfl_dir = VFL_DIR_M2M;
	v->vdev.device_caps = V4L2_CAP_VIDEO_M2M | V4L2_CAP_STREAMING;
	video_set_drvdata(&v->vdev, v);
	ret = video_register_device(&v->vdev, VFL_TYPE_VIDEO, -1);
	if (ret)
		goto release;
	ave->video = v;
	debugfs_create_file("ave-v4l2-enable", 0200, ave->debugfs, ave, &ave_video_enable_fops);
	return 0;
release:
	v4l2_m2m_release(v->m2m);
unregister:
	v4l2_device_unregister(&v->v4l2);
free:
	kfree(v);
	return ret;
}

static void ave_video_unregister(struct apple_ave *ave)
{
	struct ave_video *v = ave->video;

	if (!v)
		return;
	video_unregister_device(&v->vdev);
	v4l2_m2m_release(v->m2m);
	v4l2_device_unregister(&v->v4l2);
	kfree(v->start);
	kfree(v->process);
	kfree(v);
	ave->video = NULL;
}
