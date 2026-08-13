# Current Cidaren auth protocol notes

## Frontend constants

```text
frontend version = 2.7.0.260715_01
appId            = wx2a694105a6abbe6d
frontUrl         = https://app.vocabgo.com
distDir          = /student
```

当前 API 定义（官方 bundle）：

```text
/Auth/Thirdpart/Authorize
/Auth/Wechat/LoginByWechatCode
/Auth/Wechat/V2/LoginByWechatCode
/Auth/handshake
/Auth/Wechat/Config
/Auth/Login/Before
/Auth/Login/ByUserId
/Auth/Login
```

当前实际推荐登录是 V2。

## Official `/authorize` component

懒加载 chunk 4398 的组件极薄：

```text
currentUrl = frontUrl + distDir + "/?authorize=2"
state      = student/studentHome.deviceCode
window.location.href = open.weixin.qq.com/connect/oauth2/authorize?...&state=deviceCode
```

没有 polling、QRLogin、ScanLogin、postMessage callback bridge。

官方 `student/home` 只有当解析到 `authorize == "2"` 时才执行 `handleWechatAuthorizeLoginNew(...)`。

因此 Asterism 的手动 callback capture 使用随机 marker（!= `2`）即可避免官方页面自动消费一次性 code。`state` 也使用 Asterism 自己的 CSPRNG nonce；V2 登录请求本身不提交 state。

## Request signing

前端 POST 参数处理：

1. 原始业务参数；
2. 加 `timestamp`；
3. 加 `version`；
4. 对当前参数执行 `doSign`；
5. Axios request interceptor **之后** 再加 `app_type=1`。

因此 `app_type` 不属于签名输入。

签名：

```text
secret suffix = ajfajfamsnfaflfasakljdlalkflak
```

伪代码：

```text
keys = sorted(payload.keys())
parts = []
for key in keys:
    value = payload[key]
    if object: value = JSON.stringify(value)
    if value is truthy OR value == 0:
        parts += key + "=" + value
canonical = "&".join(parts)
sign = md5(canonical + secret_suffix)
```

前端当前 version：`2.7.0.260715_01`。

## Request headers/interceptor

POST：

```text
Content-Type: application/json;charset=UTF-8
X-Requested-With: XMLHttpRequest
Authorization-v: <deviceCode or "00">
```

无 Authorization token 时：

```text
ABC       = MD5(navigator.userAgent)
UserToken = current token or empty string
```

登录 bootstrap 可按已验证 reference 的 UA/headers 处理。

## V2 Login request

业务 payload：

```json
{
  "code": "<OAuth callback code>",
  "cpub_k": "<Base64 SPKI P-256 public key>",
  "cpub_v": "v1"
}
```

然后按上面的通用 POST 规则追加 timestamp/version/sign，再由 interceptor 追加 `app_type=1`。

Endpoint（前端相对路径语义）：

```text
/student/api/Auth/Wechat/V2/LoginByWechatCode
```

AAD 的路径部分不是带 `/Auth` 的完整 HTTP URL，而是官方 crypto 模块使用的：

```text
vcg-auth:POST:/Wechat/V2/LoginByWechatCode
```

## V2 crypto

常量：

```text
version   = v1
aadPrefix = vcg-auth
hkdfInfo  = vcg-auth-aes
curve     = P-256
HKDF      = SHA-256, 256 bits
AES       = AES-GCM, tagLength=128
```

客户端：

1. `ECDH P-256` 生成 ephemeral key pair；
2. 公钥 export 为 `SPKI` DER，再标准 Base64，作为 `cpub_k`；
3. 调 V2 Login；
4. 如果 `data.encrypted === true`：
   - `handshake.spub_k || handshake.serverPublicKey`：服务端 P-256 SPKI Base64；
   - `handshake.salt`：Base64；
   - `payload.iv`：Base64；
   - `payload.cipher_text || payload.cipherText`：Base64，包含 GCM tag；
   - `payload.aad` 必须等于 `vcg-auth:POST:/Wechat/V2/LoginByWechatCode`；
5. import 服务端 SPKI P-256 public key；
6. ECDH derive 256-bit shared secret；
7. `HKDF-SHA256(sharedSecret, salt=base64decode(salt), info="vcg-auth-aes", length=32)`；
8. AES-GCM decrypt：IV=12-byte Base64 payload IV，AAD=UTF-8 payload.aad，ciphertext=Base64 payload cipher_text；
9. plaintext UTF-8 JSON；
10. 保存 `token` 与完整 session。当前 crypto 代码后续考试域还依赖 `ucd`，因此不要只保留 token 而丢 session。

如果 `data.encrypted !== true`，官方 crypto helper会直接返回 data；实现可保留兼容，但生产应按 schema 严格验证。

## UserToken validation

Asterism 现有 Cidaren validation 已经使用 `Student/Main`。完成 V2 解密并保存 token/session 后，应立即做一次 live validation，再把 pending login 标记为 succeeded。

## Old paths

旧 `Wechat/LoginByWechatCode` 当前现场返回“请刷新页面,更新到最新的页面程序”。
旧 `/Thirdpart/Authorize` 现场微信授权失败。

不要把它们作为当前主路径。
