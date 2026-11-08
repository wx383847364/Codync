import { useEffect, useState } from 'react'
import { isImageName } from '@shared/models'
import { Icon } from '../../components/Icon'
import { Text } from '../../components/Text'
import type { OutgoingFile } from '../../store/bot-store'
import { AttachmentViewer, attachmentSymbol } from './Attachments'
import { useTr } from '../../lib/use-language'
import { analytics } from '../../store/analytics'

/** 待发送图片双击可预览，键盘激活也可打开；移除按钮独立，不触发预览或发送。 */
export function ComposerAttachment({ file, remove }: { file: OutgoingFile; remove: () => void }) {
  const tr = useTr()
  const [url, setUrl] = useState<string | null>(null)
  const [broken, setBroken] = useState(false)
  const [viewing, setViewing] = useState(false)
  /** 只记录预览使用，不包含附件名称、路径或图像内容。 */
  const preview = () => {
    analytics.track({ event: 'attachment_preview', outcome: 'opened' })
    setViewing(true)
  }

  // 缩略图和大图共用本地 URL，移除附件或卸载时统一回收。
  useEffect(() => {
    if (!isImageName(file.name)) return
    const objectURL = URL.createObjectURL(new Blob([file.data as BlobPart]))
    setUrl(objectURL)
    return () => URL.revokeObjectURL(objectURL)
  }, [file])

  if (url && !broken) {
    return (
      <span className="attachment-tile" aria-label={tr('Image attachment: {name}').replaceAll('{name}', () => file.name)} title={file.name}>
        {/* 指针双击打开；detail 为零表示键盘或辅助技术激活，不要求模拟双击。 */}
        <button className="attachment-preview" aria-label={`Preview ${file.name}`} onDoubleClick={preview} onClick={(event) => { if (event.detail === 0) preview() }}>
          {/* 解码失败退回文件名 chip，事件只携带固定失败类别。 */}
          <img src={url} alt="" onError={() => { setBroken(true); analytics.track({ event: 'attachment_preview', outcome: 'decode_failed' }) }} />
        </button>
        <button className="press attachment-remove" aria-label={`Remove ${file.name}`} title="Remove" onClick={remove}>
          <Icon name="xmark" size={11} weight="bold" color="#fff" />
        </button>
        {/* 预览只读取草稿里的图片，不产生上传或发送请求。 */}
        <AttachmentViewer source={viewing ? { file, url } : null} close={() => setViewing(false)} />
      </span>
    )
  }
  return (
    <span className="file-chip">
      <Icon name={attachmentSymbol(file.name)} size={10} color="var(--secondary)" />
      <Text size="footnote" tone="text" truncate className="file-chip-name">{file.name}</Text>
      <button className="file-chip-remove" aria-label={`Remove ${file.name}`} title="Remove" onClick={remove}>
        <Icon name="xmark" size={10} weight="bold" color="var(--secondary)" />
      </button>
    </span>
  )
}
