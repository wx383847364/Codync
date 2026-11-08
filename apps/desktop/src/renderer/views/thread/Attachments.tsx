import { analytics } from '../../store/analytics'
import { Button } from '../../components/Controls'
import { useEffect, useState } from 'react'
import { isImageName, type Attachment, type SharedFile } from '@shared/models'
import { Icon } from '../../components/Icon'
import { ModalHeader, Sheet, SheetPage } from '../../components/Overlay'
import { HStack, VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useStoreRef } from '../../store/context'
import './attachments.css'

/** The symbol for a file by its extension. */
export function attachmentSymbol(name: string) {
  const ext = name.split('.').pop()?.toLowerCase() ?? ''
  if (['png', 'jpg', 'jpeg', 'heic', 'gif', 'webp', 'tiff', 'bmp'].includes(ext)) return 'photo'
  if (ext === 'pdf') return 'doc.richtext'
  if (['zip', 'gz', 'tar', '7z'].includes(ext)) return 'doc.zipper'
  if (['mov', 'mp4', 'm4v'].includes(ext)) return 'film'
  if (['mp3', 'm4a', 'wav', 'aac'].includes(ext)) return 'waveform'
  return 'doc'
}

export function byteCount(bytes: number) {
  if (bytes < 1000) return `${bytes} bytes`
  const units = ['KB', 'MB', 'GB']
  let value = bytes / 1000
  let unit = 0
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000
    unit++
  }
  return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`
}

/** Files sent with a message: images as pictures (click for full size), other files as cards. */
export function AttachmentList({ attachments, botId }: { attachments: Attachment[]; botId: string }) {
  const [viewing, setViewing] = useState<Attachment | null>(null)
  return (
    <VStack align="flex-end" gap={4}>
      {attachments.map((file) =>
        isImageName(file.name) ? (
          <button key={file.id} className="attachment-image-button" aria-label={file.name} onClick={() => setViewing(file)}>
            <AttachmentImage source={{ file, botId }} fit="thumbnail" />
          </button>
        ) : (
          <HStack key={file.id} gap={8} className="attachment-file">
            <Icon name={attachmentSymbol(file.name)} size={16} color="var(--secondary)" />
            <VStack gap={1} shrink>
              <Text size="subheadline" tone="text" truncate className="attachment-file-name">{file.name}</Text>
              <Text size="caption2" tone="tertiary">{byteCount(file.size)}</Text>
            </VStack>
          </HStack>
        ),
      )}
      <AttachmentViewer source={viewing && { file: viewing, botId }} close={() => setViewing(null)} />
    </VStack>
  )
}

/** Where a picture's bytes are on the computer: a file sent with a message, or one a bot shared. */
export type ImageSource = { file: Attachment; botId: string } | { file: SharedFile; entryId: string }

/** 复用同一大图面板预览已发送、bot 分享及本地尚未发送的图片，本地 URL 由调用方管理。 */
export function AttachmentViewer({ source, close }: { source: ImageSource | { file: { name: string }; url: string } | null; close: () => void }) {
  const [fit, setFit] = useState<'full' | 'original'>('full')
  const sourceKey = source ? ('url' in source ? source.url : source.file.id) : null
  // 默认适应窗口，保留草稿和既有预览行为；用户可明确切换原始像素。
  useEffect(() => { if (source) { setFit('full'); analytics.track({ event: 'image_viewed', outcome: 'opened' }) } }, [sourceKey])
  return (
    <Sheet open={source !== null} onClose={close} width={1040} height={800}>
      {source ? (
        <SheetPage>
          <ModalHeader title={source.file.name} />
          <HStack gap={8} className="attachment-viewer-controls">
            {/* 原图显示仅改变缩放方式，不重新上传或重编码。 */}
            <Button onClick={() => { setFit('original'); analytics.track({ event: 'image_viewed', outcome: 'original' }) }}>100%</Button>
            <Button onClick={() => { setFit('full'); analytics.track({ event: 'image_viewed', outcome: 'fit' }) }}>Fit to window</Button>
          </HStack>
          <div className={`attachment-viewer ${fit}`}>
            {'url' in source ? <img src={source.url} alt={source.file.name} className={`attachment-image ${fit}`} /> : <AttachmentImage source={source} fit={fit} />}
          </div>
        </SheetPage>
      ) : null}
    </Sheet>
  )
}

/** 从有界缓存读取已分享图片，失败允许主动重试；卸载后释放对象 URL。 */
export function AttachmentImage({ source, fit }: { source: ImageSource; fit: 'icon' | 'thumbnail' | 'full' | 'original' }) {
  const store = useStoreRef()
  const [url, setUrl] = useState<string | null>(null)
  const [failed, setFailed] = useState(false)
  const [retry, setRetry] = useState(0)
  useEffect(() => {
    setUrl(null)
    setFailed(false)
    let revoked = false
    let made: string | null = null
    // 页面可见时读取一次，重连只补读未完成的图片。
    const read = () => {
      const data = 'entryId' in source ? store.sharedFileData(source.file, source.entryId) : store.attachmentData(source.file.id, source.botId)
      // 已卸载或换图后忽略晚到响应。
      void data.then((data) => {
        if (revoked || made) return
        if (!data) { setFailed(true); analytics.track({ event: 'image_viewed', outcome: 'failed' }); return }
        made = URL.createObjectURL(new Blob([data as BlobPart]))
        setFailed(false)
        setUrl(made)
      })
    }
    // A chat shown before the computer connects reads its pictures again once it comes online.
    let online = store.connection.kind === 'online'
    // 只在离线转在线时再次尝试读取。
    const stop = store.subscribe(() => {
      const now = store.connection.kind === 'online'
      if (now && !online && !made) read()
      online = now
    })
    read()
    // 释放订阅和对象 URL，不让缓存淘汰后的字节继续被无用视图持有。
    return () => {
      revoked = true
      stop()
      if (made) URL.revokeObjectURL(made)
    }
    // A file's id names its bytes for good; `source` is a new object every render.
  }, [store, source.file.id, retry])
  if (failed) return <div role="status"><Button onClick={() => { /* 显式重试，不循环读取失败图片。 */ setRetry((value) => value + 1) }}>Retry image</Button></div>
  if (!url) {
    return (
      <div className={`attachment-placeholder ${fit}`}>
        <Icon name="photo" size={13} color="var(--tertiary)" />
      </div>
    )
  }
  return <img src={url} alt={source.file.name} onError={() => { /* 解码失败只上报固定类别。 */ setFailed(true); analytics.track({ event: 'image_viewed', outcome: 'failed' }) }} className={`attachment-image ${fit}`} />
}
