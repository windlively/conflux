// Conflux 的 ed2k 侧车：把 goed2k 包成「行式 JSON」进程，由主程序按需拉起。
//
// 为什么是侧车而不是库：Rust 侧没有能用的 ed2k 客户端实现，goed2k（MIT）是目前
// 唯一覆盖「服务器 + Kad 找源 + 下载 + 断点数据」的成熟选择，而它是 Go 库。
//
// 协议（stdin / stdout 各一行一个 JSON，stdout 只允许出现协议行）：
//
//	请求  {"id":1,"method":"add","params":{"link":"ed2k://|file|...","dir":"/tmp"}}
//	      add 还可带 "peers":["1.2.3.4:4662"] 直接指定源（正常路径靠服务器与 Kad 找源）
//	应答  {"id":1,"ok":true,"result":{...}}
//	      {"id":1,"ok":false,"error":"..."}
//	事件  {"event":"progress","transfers":[...]}      没有 id
//	      {"event":"status","servers":2,"kad":true,"transfers":[...]}
//
// 命令：add / pause / resume / remove / list / search-start / search-state /
// search-stop / servers / shutdown
package main

import (
	"bufio"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"log/slog"
	"net"
	"os"
	"os/signal"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"syscall"
	"time"

	ed2k "github.com/monkeyWie/goed2k"
	"github.com/monkeyWie/goed2k/protocol"
)

// 默认引导资源：服务器列表取自 emule-security，nodes.dat 取自 nodes-dat，
// 与 goed2k 自带命令行里的默认值一致。
const (
	defaultServers   = "45.82.80.155:5687,176.123.5.89:4725,85.121.5.137:4232,176.123.2.239:4232,145.239.2.134:4661,91.208.162.87:4232,37.15.61.236:4232"
	defaultServerMet = "http://upd.emule-security.org/server.met"
	defaultNodesDat  = "http://www.alldivx.de/nodes/nodes.dat,https://upd.emule-security.org/nodes.dat"
	// 进度落盘与状态事件的间隔
	saveEvery = 30 * time.Second
	tickEvery = time.Second
)

type request struct {
	ID     int             `json:"id"`
	Method string          `json:"method"`
	Params json.RawMessage `json:"params"`
}

type response struct {
	ID     int         `json:"id"`
	OK     bool        `json:"ok"`
	Result interface{} `json:"result,omitempty"`
	Error  string      `json:"error,omitempty"`
}

// emitter 串行化 stdout：应答与事件来自不同 goroutine
type emitter struct {
	mu  sync.Mutex
	enc *json.Encoder
}

func (e *emitter) send(value any) {
	e.mu.Lock()
	defer e.mu.Unlock()
	_ = e.enc.Encode(value)
}

func (e *emitter) reply(id int, result any, err error) {
	if err != nil {
		e.send(response{ID: id, OK: false, Error: err.Error()})
		return
	}
	e.send(response{ID: id, OK: true, Result: result})
}

type transferJSON struct {
	Hash        string `json:"hash"`
	Name        string `json:"name"`
	Path        string `json:"path"`
	Link        string `json:"link"`
	Size        int64  `json:"size"`
	State       string `json:"state"`
	Done        int64  `json:"done"`
	Wanted      int64  `json:"wanted"`
	Received    int64  `json:"received"`
	RateBps     int    `json:"rateBps"`
	Peers       int    `json:"peers"`
	ActivePeers int    `json:"activePeers"`
	Uploaded    int64  `json:"uploaded"`
	UploadRate  int    `json:"uploadRate"`
	EtaSeconds  int64  `json:"etaSeconds"`
}

type searchResultJSON struct {
	Hash            string `json:"hash"`
	Name            string `json:"name"`
	Size            int64  `json:"size"`
	Sources         int    `json:"sources"`
	CompleteSources int    `json:"completeSources"`
	Extension       string `json:"extension"`
	Link            string `json:"link"`
}

type searchJSON struct {
	State   string             `json:"state"`
	Error   string             `json:"error,omitempty"`
	Results []searchResultJSON `json:"results"`
}

// app 把客户端与协议输出绑在一起
type app struct {
	client    *ed2k.Client
	em        *emitter
	statePath string
	log       *slog.Logger
}

func main() {
	var (
		statePath = flag.String("state", "", "状态文件路径（断点数据与 Kad 路由表）")
		logPath   = flag.String("log", "", "日志文件路径，默认与状态文件同目录")
		tcpPort   = flag.Int("tcp-port", 4661, "eD2k 监听端口")
		udpPort   = flag.Int("udp-port", 4662, "eD2k UDP 端口")
		kad       = flag.Bool("kad", true, "启用 Kad")
		upnp      = flag.Bool("upnp", false, "启用 UPnP 端口映射")
		servers   = flag.String("servers", defaultServers, "eD2k 服务器，逗号分隔")
		serverMet = flag.String("server-met", defaultServerMet, "server.met 的 URL 或路径")
		nodesDat  = flag.String("kad-nodes", defaultNodesDat, "nodes.dat 的 URL 或路径，逗号分隔")
	)
	flag.Parse()

	build := buildInfo()
	logger, logFile := newLogger(*statePath, *logPath)
	if logFile != nil {
		defer logFile.Close()
	}

	settings := ed2k.NewSettings()
	settings.Logger = logger
	settings.ClientName = "Conflux"
	settings.ModName = "Conflux"
	// 掉线重连、端口与协议行为都按 goed2k 自带命令行的生产配置来
	settings.ReconnectToServer = true
	settings.ListenPort = *tcpPort
	settings.UDPPort = *udpPort
	settings.EnableDHT = *kad
	settings.EnableUPnP = *upnp
	// 默认值只有 5 秒：eD2k 上对面排队、限速都会让几十秒没有数据，
	// 这么短的超时会把还在排队的 peer 一个个踢掉，进度就一直在原地打转
	settings.PeerConnectionTimeout = 100

	client := ed2k.NewClient(settings)
	if *statePath != "" {
		client.SetStatePath(*statePath)
	}
	if *kad {
		client.EnableDHT()
	}
	if err := client.Start(); err != nil {
		fatal(logger, fmt.Errorf("监听端口 %d 失败：%w", *tcpPort, err))
	}
	// 断点数据在 Start 之后回灌：上次没下完的分片与 Kad 路由表都从这儿回来
	if *statePath != "" {
		if err := client.LoadState(""); err != nil {
			logger.Warn("载入状态失败", "err", err)
		}
	}
	client.SetAutoSaveInterval(saveEvery)

	application := &app{client: client, em: &emitter{enc: json.NewEncoder(os.Stdout)}, statePath: *statePath, log: logger}
	// 版本与启动参数先报一条，方便主程序确认握手成功
	application.em.send(map[string]any{
		"event": "ready",
		"build": build,
		"state": *statePath,
		"kad":   *kad,
	})

	go application.bootstrap(*servers, *serverMet, *nodesDat, *kad)
	go application.forwardProgress()
	go application.tick()
	application.watchSignals()

	application.serve(os.Stdin)
	application.shutdown()
}

// serve 逐行读命令；每条命令单独起 goroutine，长命令（搜索）不会卡住暂停/继续
func (a *app) serve(input io.Reader) {
	scanner := bufio.NewScanner(input)
	scanner.Buffer(make([]byte, 0, 64*1024), 4*1024*1024)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" {
			continue
		}
		var req request
		if err := json.Unmarshal([]byte(line), &req); err != nil {
			a.em.send(response{OK: false, Error: "命令不是合法 JSON：" + err.Error()})
			continue
		}
		if req.Method == "shutdown" {
			a.em.reply(req.ID, map[string]any{"bye": true}, nil)
			return
		}
		go func(req request) {
			result, err := a.handle(req)
			a.em.reply(req.ID, result, err)
		}(req)
	}
	// stdin 关闭＝主程序没了：落盘后退出，别留下一个孤儿进程
	a.em.send(map[string]any{"event": "bye", "reason": "stdin closed"})
}

func (a *app) handle(req request) (any, error) {
	switch req.Method {
	case "add":
		var params struct {
			Link string `json:"link"`
			Dir  string `json:"dir"`
			// 手动指定的源：正常路径靠服务器与 Kad 找源，这条给「手上就有源地址」的场合用
			Peers []string `json:"peers"`
		}
		if err := json.Unmarshal(req.Params, &params); err != nil {
			return nil, err
		}
		return a.add(params.Link, params.Dir, params.Peers)
	case "pause", "resume":
		var params struct {
			Hash string `json:"hash"`
		}
		if err := json.Unmarshal(req.Params, &params); err != nil {
			return nil, err
		}
		hash, err := protocol.HashFromString(params.Hash)
		if err != nil {
			return nil, err
		}
		handle := a.client.FindTransfer(hash)
		if !handle.IsValid() {
			return nil, fmt.Errorf("没有这个任务：%s", params.Hash)
		}
		if req.Method == "pause" {
			handle.Pause()
		} else {
			handle.Resume()
		}
		_ = a.client.SaveState("")
		return a.transferOf(handle), nil
	case "remove":
		var params struct {
			Hash       string `json:"hash"`
			DeleteFile bool   `json:"deleteFile"`
		}
		if err := json.Unmarshal(req.Params, &params); err != nil {
			return nil, err
		}
		hash, err := protocol.HashFromString(params.Hash)
		if err != nil {
			return nil, err
		}
		if err := a.client.RemoveTransfer(hash, params.DeleteFile); err != nil {
			return nil, err
		}
		_ = a.client.SaveState("")
		return map[string]any{"removed": params.Hash}, nil
	case "list":
		return a.allTransfers(), nil
	case "search-start":
		var params struct {
			Query      string `json:"query"`
			KadOnly    bool   `json:"kadOnly"`
			MinSources int    `json:"minSources"`
		}
		if err := json.Unmarshal(req.Params, &params); err != nil {
			return nil, err
		}
		return a.startSearch(params.Query, params.KadOnly, params.MinSources)
	case "search-state":
		return a.searchState(), nil
	case "search-stop":
		if err := a.client.StopSearch(); err != nil {
			return nil, err
		}
		return a.searchState(), nil
	case "servers":
		return a.serverState(), nil
	default:
		return nil, fmt.Errorf("未知命令：%s", req.Method)
	}
}

func (a *app) add(linkValue, dir string, peers []string) (any, error) {
	link, err := ed2k.ParseEMuleLink(strings.TrimSpace(linkValue))
	if err != nil {
		return nil, err
	}
	if link.Type != ed2k.LinkFile {
		return nil, fmt.Errorf("这里只接受 ed2k 文件链接（ed2k://|file|…）")
	}
	// 同一个文件只留一个任务：再挂一次会拿到同一个句柄，界面与进度就对不上了
	if existing := a.client.FindTransfer(link.Hash); existing.IsValid() {
		return nil, fmt.Errorf("这个文件已经在任务里了（%s）", existing.GetFilePath())
	}
	if dir == "" {
		return nil, fmt.Errorf("没有指定保存目录")
	}
	handle, targetPath, err := a.client.AddLink(linkValue, dir)
	if err != nil {
		return nil, err
	}
	if !handle.IsValid() {
		return nil, fmt.Errorf("任务没有挂上：%s", targetPath)
	}
	a.addPeers(link.Hash, peers)
	return a.transferOf(handle), nil
}

// addPeers 把调用方指定的源直接塞进这个任务
func (a *app) addPeers(hash protocol.Hash, peers []string) {
	if len(peers) == 0 {
		return
	}
	transfer := a.client.Session().LookupTransfer(hash)
	if transfer == nil {
		a.log.Warn("指定的源没能挂上：任务不在会话里")
		return
	}
	for _, peer := range peers {
		addr, err := net.ResolveTCPAddr("tcp", strings.TrimSpace(peer))
		if err != nil {
			a.log.Warn("源地址读不出来", "peer", peer, "err", err)
			continue
		}
		if err := transfer.AddPeer(protocol.EndpointFromInet(addr), int(ed2k.PeerServer)); err != nil {
			a.log.Warn("加源失败", "peer", peer, "err", err)
		}
	}
}

func (a *app) startSearch(query string, kadOnly bool, minSources int) (any, error) {
	query = strings.TrimSpace(query)
	if query == "" {
		return nil, fmt.Errorf("搜索词不能为空")
	}
	scope := ed2k.SearchScopeAll
	if kadOnly {
		scope = ed2k.SearchScopeDHT
	}
	// 上一次搜索没结束就先停掉：界面上同时只有一份结果
	_ = a.client.StopSearch()
	if _, err := a.client.StartSearch(ed2k.SearchParams{
		Query:      query,
		Scope:      scope,
		MinSources: minSources,
	}); err != nil {
		return nil, err
	}
	return a.searchState(), nil
}

func (a *app) searchState() searchJSON {
	snapshot := a.client.SearchSnapshot()
	out := searchJSON{
		State:   string(snapshot.State),
		Error:   snapshot.Error,
		Results: make([]searchResultJSON, 0, len(snapshot.Results)),
	}
	for _, result := range snapshot.Results {
		out.Results = append(out.Results, searchResultJSON{
			Hash:            result.Hash.String(),
			Name:            result.FileName,
			Size:            result.FileSize,
			Sources:         result.Sources,
			CompleteSources: result.CompleteSources,
			Extension:       result.Extension,
			Link:            result.ED2KLink(),
		})
	}
	return out
}

func (a *app) serverState() any {
	connected := a.client.Session().ConnectedServerIDs()
	dht := a.client.DHTStatus()
	return map[string]any{
		"serversConnected": len(connected),
		"servers":          connected,
		"kadBootstrapped":  dht.Bootstrapped,
		"kadLiveNodes":     dht.LiveNodes,
		"kadKnownNodes":    dht.KnownNodes,
		"kadFirewalled":    dht.Firewalled,
		"transfers":        a.allTransfers(),
	}
}

func (a *app) allTransfers() []transferJSON {
	handles := a.client.Transfers()
	out := make([]transferJSON, 0, len(handles))
	for _, handle := range handles {
		if handle.IsValid() {
			out = append(out, a.transferOf(handle))
		}
	}
	return out
}

func (a *app) transferOf(handle ed2k.TransferHandle) transferJSON {
	status := handle.GetStatus()
	hash := handle.GetHash()
	path := handle.GetFilePath()
	return transferJSON{
		Hash:        hash.String(),
		Name:        filepath.Base(path),
		Path:        path,
		Link:        ed2k.FormatLink(filepath.Base(path), handle.GetSize(), hash),
		Size:        handle.GetSize(),
		State:       stateOf(status),
		Done:        status.TotalDone,
		Wanted:      status.TotalWanted,
		Received:    status.TotalReceived,
		RateBps:     status.DownloadRate,
		Peers:       status.NumPeers,
		ActivePeers: handle.ActiveConnections(),
		Uploaded:    status.Upload,
		UploadRate:  status.UploadRate,
		EtaSeconds:  status.ETA,
	}
}

// stateOf 把 goed2k 的状态压成主程序认识的那几个词
func stateOf(status ed2k.TransferStatus) string {
	if status.Paused {
		return "paused"
	}
	switch status.State {
	case ed2k.Downloading:
		return "downloading"
	case ed2k.Finished:
		return "completed"
	case ed2k.Verifying:
		return "verifying"
	case ed2k.PausedState:
		return "paused"
	default:
		return "queued"
	}
}

// forwardProgress 把 goed2k 的进度订阅转成事件推给主程序
func (a *app) forwardProgress() {
	events, cancel := a.client.SubscribeTransferProgressBuffered(16)
	defer cancel()
	for event := range events {
		changed := make([]map[string]any, 0, len(event.Transfers))
		for _, item := range event.Transfers {
			changed = append(changed, map[string]any{
				"hash":        item.Hash.String(),
				"name":        item.FileName,
				"path":        item.FilePath,
				"state":       stateOf(ed2k.TransferStatus{State: item.State, Paused: item.Paused}),
				"done":        item.TotalDone,
				"wanted":      item.TotalWanted,
				"received":    item.TotalReceived,
				"peers":       item.NumPeers,
				"activePeers": item.ActivePeers,
				"removed":     item.Removed,
			})
		}
		if len(changed) > 0 {
			a.em.send(map[string]any{"event": "progress", "transfers": changed})
		}
	}
}

// tick 每秒推一次完整状态：进度事件里没有速率与 ETA，卡片要靠它；
// 服务器与 Kad 数字也在这儿报，主程序的统计只认这两个字段
func (a *app) tick() {
	ticker := time.NewTicker(tickEvery)
	defer ticker.Stop()
	for range ticker.C {
		dht := a.client.DHTStatus()
		a.em.send(map[string]any{
			"event":     "status",
			"servers":   len(a.client.Session().ConnectedServerIDs()),
			"kadLive":   dht.LiveNodes,
			"kadKnown":  dht.KnownNodes,
			"transfers": a.allTransfers(),
		})
	}
}

func (a *app) bootstrap(servers, serverMet, nodesDat string, kad bool) {
	if nodesDat != "" && kad {
		sources := splitList(nodesDat)
		if err := a.client.LoadDHTNodesDat(sources...); err != nil {
			a.log.Warn("载入 nodes.dat 失败", "err", err)
		}
	}
	if serverMet != "" {
		sources := splitList(serverMet)
		if err := a.client.ConnectServerMet(sources[0]); err != nil {
			a.log.Warn("连接 server.met 失败", "err", err)
		}
	}
	if servers != "" {
		if err := a.client.ConnectServers(splitList(servers)...); err != nil {
			a.log.Warn("连接服务器失败", "err", err)
		}
	}
	a.log.Info("引导完成")
}

func (a *app) watchSignals() {
	signals := make(chan os.Signal, 1)
	signal.Notify(signals, syscall.SIGINT, syscall.SIGTERM)
	go func() {
		<-signals
		a.em.send(map[string]any{"event": "bye", "reason": "signal"})
		a.shutdown()
		os.Exit(0)
	}()
}

func (a *app) shutdown() {
	if a.statePath != "" {
		if err := a.client.SaveState(""); err != nil {
			a.log.Warn("落盘失败", "err", err)
		}
	}
	a.client.Close()
}

func splitList(value string) []string {
	parts := strings.Split(value, ",")
	out := make([]string, 0, len(parts))
	for _, part := range parts {
		if trimmed := strings.TrimSpace(part); trimmed != "" {
			out = append(out, trimmed)
		}
	}
	return out
}

// newLogger 默认把日志写到状态文件旁边：stdout 只能走协议
func newLogger(statePath, logPath string) (*slog.Logger, *os.File) {
	if logPath == "" && statePath != "" {
		logPath = filepath.Join(filepath.Dir(statePath), "ed2k-sidecar.log")
	}
	if logPath == "" {
		return slog.New(slog.DiscardHandler), nil
	}
	if err := os.MkdirAll(filepath.Dir(logPath), 0o755); err != nil {
		return slog.New(slog.DiscardHandler), nil
	}
	file, err := os.OpenFile(logPath, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return slog.New(slog.DiscardHandler), nil
	}
	return slog.New(slog.NewTextHandler(file, &slog.HandlerOptions{Level: slog.LevelInfo})), file
}

func fatal(logger *slog.Logger, err error) {
	logger.Error("侧车启动失败", "err", err)
	// 启动失败也要按协议回一条，主程序才知道为什么没有 ready
	(&emitter{enc: json.NewEncoder(os.Stdout)}).send(map[string]any{"event": "fatal", "error": err.Error()})
	os.Exit(1)
}

func buildInfo() string {
	return fmt.Sprintf("ed2k-sidecar %s/%s", runtime.GOOS, runtime.GOARCH)
}
