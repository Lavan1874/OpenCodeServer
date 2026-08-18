# OpenCodeServer 产品边界（讨论结论）

本文只记录已经确定的产品行为。实现细节、测试步骤和 Apple API 依据继续放在 `AGENTS.md`、`docs/adr/` 和后续设计文档中。

## 产品定位

- OpenCodeServer 是一个极简、原生、符合现代 macOS 规范的菜单栏工具。
- 它只负责配置、启动、监督和观察一个 OpenCode，不干预 OpenCode 的项目、模型、Provider、Agent、MCP、插件或会话。
- 第一版面向具备基本 macOS 和命令行能力的用户，不做首次运行向导；将来公开发布时再考虑。
- 产品路线为“个人使用优先、未来小范围源码分发就绪”：第一版保持极简、个人 Apple Development 签名和手动安装，同时稳定标识、当前配置结构、当前进程协议和安全边界。
- 将来可能在 GitHub 开源并分享给少量熟悉 macOS 的用户；取得 Developer ID 和进行 Apple 公证的可能性很低，不将其作为产品或构建流程的前提。
- GitHub 分发时希望同时提供预编译版本；未公证版本首次运行所需的 Gatekeeper 人工确认可以接受。
- OpenCodeServerAgent 只管理一个 OpenCode 实例。端口被其他进程占用时只报告，不接管或终止其他进程。

## 两个独立组件

- `OpenCodeServer`：纯菜单栏 GUI，不显示 Dock 图标。
- `OpenCodeServerAgent`：由 macOS `launchd` 管理的独立 OpenCode 管理器，OpenCodeServer 不运行时也可以继续工作。
- OpenCodeServer 和 OpenCodeServerAgent 的登录启动分别配置，互不依赖。
- OpenCodeServer 意外崩溃或被系统终止时，OpenCodeServerAgent 和 OpenCode 继续运行。
- macOS 在登录时独立启动 OpenCodeServer 和 OpenCodeServerAgent，不假设二者的启动顺序；OpenCodeServer 通过当前 IPC 协议的订阅推送监视状态，断线后按有界退避重连，并允许 OpenCodeServerAgent 稍后可达。
- OpenCodeServer 不检查或管理 OpenCode PID，不直接发送进程信号，不依据短暂 IPC 不可达自动注销或重新注册 OpenCodeServerAgent。
- 同一 `CFBundleVersion` 下，已启用的 OpenCodeServerAgent 暂时不可达只改变 OpenCodeServer 的展示状态；只有首次注册、真实 Bundle 升级、明确注册错误或用户显式选择“Repair OpenCodeServerAgent”才进入 Service Management 修改流程。
- 真实 Bundle 升级属于一次持久化且有上限的更新事务：仅为规避已在 macOS 26 真机复现的陈旧 Background Task Management 启动元数据，最多执行三次经过状态观察的注销/注册尝试，每次都必须等待真实 IPC 验证；事务尝试次数跨 OpenCodeServer 重启保持，不得把这套重试用于同版本日常状态监视。
- `opencodeserverctl` 只使用与 OpenCodeServer 相同的 IPC 协议，不直接读取或修改 OpenCodeServerAgent 运行状态文件。
- 菜单提供两个明确动作：
  - “退出 OpenCodeServer”：OpenCodeServerAgent 和 OpenCode 继续运行。
  - “停止 OpenCode 并退出 OpenCodeServer…”：提醒可能中断任务，确认后停止 OpenCode 并注销 OpenCodeServerAgent。

## 菜单与设置

- 菜单只显示状态和常用操作；完整配置放在独立的原生设置窗口。
- 菜单遵循渐进披露（NN/g）与 HIG”菜单项集合稳定”两条基线（原文存 `~/Documents/OpenCodeServer-References/`）：健康、运行时长、监听地址、版本四行常驻；OpenCodeServerAgent 注册状态、FDA、密码授权、认证、配置待重启五行只在偏离正常值时出现（OpenCodeServerAgent 不可达时全部显示，避免陈旧值看起来像一切正常）；此时凡是只能由 OpenCodeServerAgent 证明的值必须显示为“无法判断”或等价的未知状态，不得把缺少 IPC 数据误报成“密码未配置”“认证未启用”等确定的否定结论；当前存在可操作的配置错误或运行错误时，额外显示一行 `Detail:` 说明，错误消失后立即隐藏；动作项集合永远不变，只置灰不隐藏。
- Start、Stop、Restart、Continue Waiting、Force Stop 五个动作的可用性由 OpenCodeServerAgent 在 `Status.action_capabilities` 中根据进程身份、凭据和运行状态耐久事实计算；OpenCodeServer 只映射该值（另保留刚保存凭据尚未收到确认时对 Start/Restart 的本地保护），不从粗粒度 `server_state` 重建前置条件。Continue Waiting 与 Force Stop 仅在优雅停止超时后由 GUI 提供；`opencodeserverctl --force` 仍按命令层显式语义执行。
- 不常用的恢复与检查动作（Open Logs、Recheck FDA、打开 FDA 设置、打开登录项设置、Repair OpenCodeServerAgent…）收进单层 `Advanced` 子菜单。
- 设置窗口同样渐进披露：mDNS 与可执行文件选择收进 `Advanced` 折叠区；已加载配置用到非默认值时自动展开，已配置的值永不可见丢失。折叠区以分节头呈现——上方细隔线 + chevron + 半粗标题的整行可点击行（参照 Finder 显示简介），并向 VoiceOver 暴露展开/收起状态。窗口不常驻成段说明文字：唯一常驻文案是密码字段正下方一行状态中立的小号 caption（"Without a password, OpenCode is unauthenticated."，同时承担 Remove… 的后果说明）；两进程授权模型等其余说明由对应控件的 help tag 在悬停时承载。
- mDNS 缺省关闭；只有用户在 `Advanced` 中明确启用并让新配置随 OpenCode 重启生效后，才允许触发 macOS 本地网络隐私授权。Local Network 归属首先是签名与责任链的前置条件，而不是靠 UI 文案、改可执行文件名或继续堆叠 Bundle 结构补丁来保证的结果。每个候选版本必须先验证：SMAppService 的运行时记录含 `parent bundle identifier = ai.opencode.server`，所有 arm64 Mach-O（包括 Agent 和外来 OpenCode）都有非空且互不重复的 `LC_UUID`，并且同一输入的重复构建保持稳定。由 OpenCodeServerAgent 启动外来 OpenCode 子进程的组合已经双签名模型干净状态实测（2026-08-16 双组对照，ADR 0018 修订 / ADR 0021 实施后测量）：无论旧的 self-signed 还是当前 Apple Development（含 Team ID）身份，系统授权框与“隐私与安全性 → 本地网络”条目均命名为 `OpenCodeServerAgent`。这种 UI 归属不可达是平台限制，与签名身份无关，不再作为本架构的代码修复目标，也不把它伪装成已通过。仅 Team ID 变化的签名模型更换不再触发重新验收；只有责任链变化时才必须在干净隐私状态重新验收系统授权框和“隐私与安全性 → 本地网络”条目，并再决定产品承诺。
- Build 66 Agent-only 诊断进一步确认：即使不启动外部 OpenCode，直接由 OpenCodeServerAgent 发起 multicast，本地网络弹窗仍显示 `OpenCodeServerA`；Unified Log 同时记录 `No team ID found` 和 Agent 的实际路径。因此“仅仅是外部孙进程导致归属错误”的假设不成立；当时把 self-signed/no-Team-ID 的签名强度或责任链列为首要未决因素，2026-08-16 双组干净状态实验（ADR 0018 修订）已定论：签名身份不是 Local Network UI 归属的区分因素，两种签名模型下授权框与设置条目均显示 `OpenCodeServerAgent`，平台限制在于 LaunchAgent 启动外来子进程的责任链结构。该结果只适用于本次 macOS 26.6.1 测试组合，不把 Developer ID 必然修复所有责任链问题说成事实。
- 机制层判断：`SMAppService` 运行时记录中的 `parent bundle identifier` 只说明 launchd 服务由哪个主 App 注册和管理；它不等于 Local Network 操作的 responsible-code 归属，也不会自动把由 OpenCodeServerAgent 启动的外部 OpenCode 网络操作向上归因到 OpenCodeServer。两条链路必须分别验收：前者用 Service Management 状态和认证 IPC 验证，后者用干净隐私状态下的系统授权框与“隐私与安全性 → 本地网络”条目验证。看到 `parent bundle identifier = ai.opencode.server` 不能据此宣称 Local Network 归属已经解决，也不应继续仅靠调整 Bundle 结构反复尝试。
- 设置窗口的配置表单遵循原生 macOS 双列表单：标签向控件列尾端对齐，所有控件共用稳定的起始线，主表单与 `Advanced` 共用按当前系统字体实际测得的标签列宽；Password 行的检查、等待、已存储、编辑和删除状态只能改变该行内部内容，绝不重排其他字段。该行为两行结构：内容行（状态文本、活动指示器或密码输入框）占满控件列全宽，Show/Edit/Copy/Remove 控件居第二行；行高按最高语义状态的两行组合从控件自身 fitting size 预留，与量测时刻的可见状态无关；无任何可见控件的态（检查/等待）整体隐藏第二行使其间距坍塌，单行内容在预留行高内居中；无文本基线的小型活动指示器与行标签按圆心/视觉中心对齐，不参与基线对齐。可编辑 `NSTextField` 不具备可用的横向 intrinsic/fitting width，Port、完整 IPv6 地址和 executable 输入量一律用 AppKit `sizeThatFits(_:)` 的 cell-backed 尺寸衡量；公共控件列和窗口宽度再与最宽 Password、Agent access、Startup 原生状态共同推导，不使用截图调出来的固定字段或窗口宽度。动态反馈文本改变换行高度时只重算窗口高度，宽度和表单列保持不动；两个登录选项归入一个 `Startup` 行，不重复显示组件名标签。
- `Edit…` 或 `Copy` 等待 login keychain 时只在 Password 行的固定位置显示原生小型活动指示符，Security.framework 调用继续在工作线程执行。用户以 Cancel、Escape 或 Command–Period 放弃系统授权是正常取消：直接回到 `Stored in Keychain`，不显示红色错误；其他钥匙串错误使用可操作的人话，原始 OSStatus 只进 Unified Logging。
- OpenCode 主状态使用四态并配文字，不能只靠颜色：
  - 绿色：健康运行。
  - 黄色：正在启动、停止、重启，或进程存在但尚未健康。
  - 红色：明确故障。
  - 灰色：主动停止、未启用或无法可靠判断。
- FDA、认证、配置待重启和版本待重启是独立信息，不改变健康灯颜色。
- 用户名只在设置窗口显示，不进菜单。
- 不显示项目列表、FileProviderDomain 状态、崩溃计数、子网或防火墙状态。

## 配置边界

- 菜单和外部 `config.plist` 是同一份配置的两个入口（密码除外，见“认证”）。
- 可配置监听地址、端口、用户名、密码、mDNS 和 OpenCode 可执行文件。
- 不读取或修改 `opencode.json`。
- OpenCode 运行时保存配置不会自动重启；新配置在下一次 OpenCode 重启后生效，并显示“配置待重启生效”。
- 通过 SSH 直接编辑配置文件具有完全相同的语义（密码只能经设置窗口写入钥匙串）。
- 配置无效不影响当前正在运行的 OpenCode；下次 OpenCode 启动时明确报错。
- 产品缺省配置采用安全通用值：`127.0.0.1:4096`、mDNS 关闭、密码为空。
- 只接受并生成当前 `SchemaVersion` 的完整配置；不识别、迁移、清理或保留旧配置结构，也不把个人局域网配置写进 App Bundle。

## OpenCode 路径与版本

- 自动发现常见位置和当前环境 `PATH` 中的多个 OpenCode，并在设置中列出候选项。
- 用户可通过原生文件选择器指定其他任意路径。
- 允许稳定符号链接；保存 Homebrew 前端路径，不保存版本化 Cellar 目标。
- 最终目标必须是可执行的原生 Mach-O 文件，不允许脚本包装器。
- 不绑定 OpenCode 二进制哈希；Homebrew 更新后路径仍有效。
- 第一版只比较“正在运行的版本”和“当前安装的版本”。不自动重启，不执行 `brew update` 或 `brew upgrade`。
- 用户选择的 OpenCode 必须是受信任的原生程序；允许选择任意路径不等于承诺隔离敌对 Mach-O。第一版不引入 EndpointSecurity、wrapper、guardian 或额外 entitlement 来构造通用进程沙箱。
- “当前安装的版本”是纯信息项。版本查询异常不得影响 OpenCode 健康、监督或 IPC；若查询进程被观察到逃离专用进程组或身份检查异常，本次查询失败并对同一路径熔断自动重试，直到路径改变或 OpenCodeServerAgent 重启。
- 远端更新检查暂不实现，只保留将来加入只读、手动检查的可能。

## 认证

- 保持 OpenCode 原生行为：有密码时启用 HTTP Basic Auth；无密码时允许 OpenCode 正常启动且不认证。
- 密码只存 login keychain（Generic Password，service `ai.opencode.server`，account 为有效用户名），不再写入 `config.plist` 或任何落盘文件；OpenCodeServer 负责创建、原地更新和删除条目，OpenCodeServerAgent 只读。
- 密码变更一律原地更新钥匙串条目。真实改密后 GUI 必须给 OpenCodeServerAgent 发非交互的 `credential_changed` 通知（否则 OpenCodeServerAgent 会一直沿用内存里的旧密码，"重启生效"会静默地用旧密码重启 OpenCode——v47 走查实证）；显式删除则发送独立的 `credential_removed`，因为成功的 `SecItemDelete` 已证明条目不存在。改密时 OpenCodeServerAgent 翻为 `access_pending`（运行中的进程继续用旧密码、保持受管），Save 自身绝不请求授权；随后的重读有两条路径——授权标记记录了相同 Team ID 时由 OpenCodeServerAgent 在有界 worker 上自动静默重读一次（team 锚定签名下实测无弹窗，见 ADR 0016 2026-08-17 两条修订），否则由用户点击 “Allow Keychain Access…” 在点击上下文中重新授权；删除时 OpenCodeServerAgent 直接收敛为 `not_configured`，保留运行中 OpenCode 的旧配置直到用户明确重启，新启动使用无密码配置。内容未变的保存必须是空操作，不得触发条目更新。（历史：自签时代 macOS 26 原地更新会清空 XARA `partition_id` 授权名单（2026-08-05 实测），真实改密必然撤销授权；Apple Development team 锚定签名下不再复现。）
- OpenCodeServerAgent 的常规读取一律禁止系统弹窗。2026-08-09 的隔离实验在 macOS 26.5.1（25F80）、本地自签名且无 Team ID、`SecAccess` 创建的 Generic Password 和 login keychain version 512 这一当时的产品配置下确认（签名模型此后经 ADR 0021 迁移为 Apple Development）：应用 ACL 中互相兼容的 Designated Requirement 不足以保证静默解密；同路径原子替换为不同 cdHash 的新 Reader 后，login keychain 的独立 `partition_id` 检查仍要求一次新授权。该结论不推广到所有 file-based keychain 实现；实验中的临时 version-256 钥匙串没有表现出同样行为。持久化授权标记记录账号、构建版本号与签名 Team ID 三元组：版本精确匹配才允许常规后台静默读；team 匹配（版本不匹配或改密后）允许每个进程运行期内一次自动静默重读，失败即回退点击授权，绝不替未经成功解密验证的情形声称已有授权；全新条目、team 不匹配、自签/临时签名遗留标记一律留在显式点击路径。2026-08-17 真机观测（Build 75→76 同 team 升级、`SecItemUpdate` 改密后重读、全新 agent 进程重启，三轮均静默无弹窗）支撑了这一放宽；若最低 macOS、签名模型、钥匙串实现或存储方案改变，必须重新验证；ADR 0021 phase-5 证书续签实验（2026-08-28）是下一个既定重验点，若续签破坏了 team 锚定授权，本规则收窄回纯点击。
- 后台路径的解密级读取永不内联在监督事件循环上：一律走单 flight 有界 worker，保证弹窗或 securityd 延迟不会卡死进程监督或烧掉 SMAppService 注册事务尝试。
- 凭据变更 journal、Keychain 授权标记、config.plist 等私有状态文件一律经有界上限、`O_NOFOLLOW`、fstat 校验（属主、权限、常规文件）的读取器读取；不安全或不可读的文件绝不被当作“不存在”而覆盖写默认值，也绝不构成授权证据。journal 无法安全读取时 GUI 不再启动即崩溃，而是以“凭据变更不可用”降级模式运行：显示具体原因、提供显式重试，凭据动作在恢复前明确报错而不静默排队（见 ADR 0016）。
- 打开设置窗口只在后台线程执行不解密的属性探测，并显示“已存储在钥匙串”或空密码输入框；打开窗口本身绝不读取密码，也绝不触发系统钥匙串弹窗。已有密码只有在用户明确点击 `Edit…` 或 `Copy` 后才由 OpenCodeServer 在后台线程执行解密级读取；`Show` 只在进入编辑态后出现。删除已有密码使用明确的 `Remove…` → Save 状态，不把空字段猜成删除意图。用户名改变且要沿用已有密码时，必须先点击 `Edit…`，避免 Save 为迁移密码而暗中解密。所有 Security.framework 查询、读取、创建、更新和删除都离开 AppKit 主线程执行；Save 使用显式编辑时已经取得的原值判断 unchanged，不为比较而再次解密。
- 用户名变更（账户迁移）的保存是显式迁移事务，顺序固定为：创建新账户钥匙串项 → 保存 `config.plist` → 删除旧项（旧项删除失败不阻塞保存，清理可重试）；全程五阶段耐久 journal（`staged → newCredentialReady → configurationSaved → cleanupOld → 完成`），崩溃后按“当前配置指向 + 仅属性探测”恢复，恢复路径永不解密、永不弹授权框，每次删除前重读配置复核账户；同账户改密/删除维持配置先行的简单事务，改用户名同时待删除密码的组合在 UI 直接拒绝（见 ADR 0016 2026-08-17 修订）。
- 首次创建非空密码或真实改密后，如果 OpenCode 正在运行，Save 立即只给出一个与当前任务相关的对话框：授权未完成时主按钮为 “Allow & Restart”——只有用户点击它才请求系统钥匙串授权（Save 自身绝不触发授权弹窗），授权成功后 OpenCodeServer 观察到 OpenCodeServerAgent 回到 `configured` 便自动重启，无需第二个对话框，也不遗留“待重启”状态；选择 “Later” 则不中断当前工作。team 锚定签名下授权读取通常已由 OpenCodeServerAgent 在后台静默自动完成（ADR 0016 第二条 2026-08-17 修订），此时该点击实际只决定立即重启与否——重启决策永远留给用户。如果 OpenCode 没有运行，不弹重启对话框，只在设置窗口渐进披露 `Agent access` 状态与 “Allow Keychain Access…” 按钮。授权已完成时维持原 “Restart OpenCode to apply the changes?” 询问。
- 保存配置且 OpenCode 正在运行时，设置窗口提示 “Restart OpenCode to apply the changes?”（【Restart OpenCode】/【Later】）；选择稍后不会进入死局：OpenCodeServerAgent 将身份核验通过但配置过期的进程接管为受管进程（可停止、可重启，状态显示 `Restart pending`），用户随时可通过 “Restart OpenCode…” 收敛。
- 菜单密码行只在授权待处理时出现（`Access not granted — open Settings`）；已授权或未配置不占用菜单行。
- OpenCodeServerAgent 未获授权时拒绝启动 OpenCode 并给出明确指引，不静默降级为无认证启动。
- 用户名未设置时沿用 OpenCode 默认值 `opencode`。
- 健康但未启用认证时状态灯仍为绿色；仅当监听地址为非 loopback 时菜单另行显示“认证：未启用”（未认证的 loopback 监听是文档承认的默认形态，不占菜单行）。
- 菜单永不泄露真实密码长度（密码行只在授权待处理时出现）；设置窗口允许临时显示或复制密码。
- opencodeserverctl、状态、日志和进程参数不得输出密码。
- 忘记密码时在设置窗口直接重输保存即可；健康检查 401 时按提示重存密码并重启 OpenCode。
- OpenCodeServer、OpenCodeServerAgent 与 opencodeserverctl 只实现同一个当前 IPC 协议版本；不提供跨版本兼容、降级或兼容层。

## FDA 与 File Provider

- FDA 由 OpenCodeServerAgent 通过最小只读功能探测验证，不查询或修改 TCC 数据库。
- FDA 使用三态：“已验证 / 未验证 / 无法判断”。
- FDA 为“未验证”或“无法判断”时仍允许启动 OpenCode，但必须明确提示权限风险，不把探测结果作为强制启动条件。
- OpenCodeServerAgent 启动时自动验证；打开 OpenCodeServer 菜单或设置、从系统设置返回时按需刷新；同时提供手动重新验证，不持续轮询。
- 设计目标是 TCC 将 OpenCodeServerAgent 和其启动的 OpenCode 向上归因到稳定的 OpenCodeServer，使用户只需给 OpenCodeServer 授权。
- 第一版不针对 FileProviderDomain 探测或显示状态，暂以“FDA 已验证即可访问 File Provider 路径”为产品假设；若真实测试不符再单独设计。

## OpenCode 监督与恢复

- OpenCodeServerAgent 直接管理一个 OpenCode 进程及其进程组。
- OpenCodeServerAgent 意外崩溃但 OpenCode 仍存活时，新 OpenCodeServerAgent 在严格验证 PID、启动时间、路径和健康端点后重新接管；不能确认时不杀进程。
- 用户主动停止、重启 OpenCode 或选择“停止 OpenCode 并退出 OpenCodeServer”时，只提醒可能中断任务，不分析 OpenCode 会话，确认后的结果由用户负责。
- 停止先尝试优雅退出。超时后不自动强杀，用户可选择继续等待或强制停止；opencodeserverctl 只有显式 `--force` 才强制结束。
- OpenCode 意外退出时自动恢复，暂时硬编码约 `1、2、5、15、30` 秒的五次重试；稳定运行一段时间后清零。
- 故障通知形成闭环：首次异常通知一次，随后只在恢复成功或最终停止重试时再通知一次。
- HTTP 健康检查由 OpenCodeServerAgent 请求 OpenCode 官方 `/global/health` 接口，只验证 HTTP 可用性、`healthy` 和版本，不读取会话或项目。
- 健康检查连续失败时变黄并通知，但不单凭健康检查失败自动重启；只有进程真正退出才进入自动恢复。
- 专用进程组负责正常、协作式 OpenCode 进程树。已观察到的进程组逃逸或身份异常必须 fail-closed，不得接管、误杀或向新的进程组发信号。
- 直接子进程退出后不立即丢弃进程组：以未 reap 的 `Child` 为锚，对授权进程组只发一次协作 SIGTERM 并在优雅窗口内等待收敛（超时不自动 SIGKILL，强制终止仍是显式用户动作），组为空且运行状态记录清除耐久后才允许自动恢复重启；重启后缺失 leader 的记录只读观察，不再有信号授权（ADR 0015 2026-08-17 修订）。
- 显式 Start/Stop/Restart 是运行状态持久化的事务边界：期望状态未耐久写入即返回 IPC 错误且不触碰进程；写入结果不确定（rename 已可见、目录同步未证实）时保留新意图，由有界重试耐久后再执行动作。
- 运行状态文件不可读时绝不回退到默认值（默认期望态为 Running，静默采纳等于重启已被用户停止的 OpenCode），不覆盖可能仍描述活进程的记录；IPC 保持可用以呈现具体故障。
- OpenCode 启动为两阶段事务：先耐久写入 `launch_pending` 标记再创建子进程，进程记录与标记清除由一次原子写提交；重启后发现未完结标记的 OpenCodeServerAgent 拒绝启动第二个 OpenCode，等待显式处理。
- HTTP 健康检查在单飞行 worker 上执行，DNS 与网络 I/O 永不阻塞监督事件循环；结果须经完整进程身份、配置指纹与单调代数校验后方可应用（A→B→A 循环不复活旧结果，ADR 0009 Addendum 4）。
- “最后一次可靠观察之后主动 `setsid`、重建会话并重父化”的敌对后代属于第一版明确接受的信任边界外场景和残余风险，不再作为发布阻断项。若未来要运行不可信任的任意 Mach-O，必须单独立项设计系统级隔离，不能继续给当前监督器叠加启发式 containment。

## opencodeserverctl 与无人值守

- 提供精简的 `opencodeserverctl`，让用户通过 SSH 向 OpenCodeServerAgent 发送 IPC 命令，不要求 OpenCodeServer 运行。
- 第一版范围：`status`、`start`、`stop`、`restart`、`logs`、`version`、`status --json` 和只读配置校验。
- opencodeserverctl 不负责修改配置；需要时直接编辑 `config.plist`。
- opencodeserverctl 不输出密码。

## 日志与通知

- OpenCodeServer 自身使用 macOS Unified Logging，可通过“控制台”App和 `log` 命令查看，不另造复杂日志轮转系统。
- 使用固定 subsystem 和少量 category，规范区分 debug、info、notice、error、fault。
- 记录生命周期、退出原因、配置错误、健康状态和重试历史。
- 不记录密码、认证头、完整环境、配置全文、提示词、会话内容或文件内容。
- 默认不主动开启 OpenCode 的详细调试日志。
- 系统通知只用于需要用户注意的异常；正常启动、停止和重启不通知。
- 首次运行时简要解释异常通知用途并请求授权；拒绝不影响核心功能。
- OpenCodeServerAgent 为每条异常通知事件生成独立的全局唯一 UUIDv4 `event_id`；事件身份不使用会跨进程生命周期回退的递增编号，也不引入 generation/sequence。OpenCodeServer 只按不透明 `event_id` 去重，并在 macOS 接受通知请求后才把它写入有界的近期已提交集合；同一 `event_id` 同时作为系统通知请求标识。当前状态协议只保留最新事件，不承诺 OpenCodeServer 离线期间的历史事件补发；若未来需要补发，必须另行决定持久事件队列和确认协议。

## 首次运行与未来扩展

- 第一版不提供设置向导：自动创建默认配置、检测 OpenCode、注册组件并尝试启动；系统权限仍由用户自行处理。
- 首次启动时自动打开一次设置窗口（HIG Onboarding：在首次运行时把设置入口摆到用户面前，而不是留给一个灰色图标）；UserDefaults 标记保证只发生一次，之后永不自动弹出。这不是向导，只是单次入口展示。
- 不启用 App Sandbox；从第一版启用 Hardened Runtime，并坚持最小 entitlement 和最小权限。
- 开发和发布流程不以 Developer ID 或公证为前提，但 App Bundle、嵌套可执行文件、签名顺序和 entitlement 从第一版起保持符合未来 Developer ID 签名与公证的结构要求；不得为此增加当前产品功能或用户流程的复杂度。
- 第一阶段允许在用户自己的多台 Apple Silicon Mac 上测试，但不面向公众分发；暂不支持 Intel Mac。
- 第一阶段最低支持版本为 macOS 26。
- 正式测试和日常使用统一安装到 `/Applications/OpenCodeServer.app`；不使用 `~/Applications`，注册后台服务后保持安装路径稳定。
- 指定一台开发 Mac 保存当前 Apple Development 私钥并统一签署 Release 验收构建；Debug、测试和 CI 继续使用 ad hoc；其他测试 Mac 只安装已签名构建和公共证书，不持有签名私钥。
- 每台 Mac 使用自己的外部配置，并分别完成 FDA、后台项目和通知授权，权限状态不在机器之间迁移。
- 暂不实现 FileProviderDomain 探测、远端版本检查、多实例、自动 Homebrew 更新、配置重试参数 UI 或面向小白用户的引导。
- 将来是否增加这些功能，以真实需求和测试结果为准。
