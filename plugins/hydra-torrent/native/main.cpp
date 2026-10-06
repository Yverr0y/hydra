// SPDX-License-Identifier: MIT OR Apache-2.0
#include "hydra-native.h"
#include <libtorrent/add_torrent_params.hpp>
#include <libtorrent/alert_types.hpp>
#include <libtorrent/load_torrent.hpp>
#include <libtorrent/magnet_uri.hpp>
#include <libtorrent/read_resume_data.hpp>
#include <libtorrent/session.hpp>
#include <libtorrent/settings_pack.hpp>
#include <libtorrent/torrent_info.hpp>
#include <libtorrent/version.hpp>
#include <libtorrent/write_resume_data.hpp>
#include <nlohmann/json.hpp>

#include <algorithm>
#include <atomic>
#include <chrono>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <limits>
#include <set>
#include <sstream>
#include <stdexcept>
#include <thread>
#include <map>
#include <libtorrent/peer_info.hpp>
#include <libtorrent/ip_filter.hpp>
#ifdef _WIN32
#include <windows.h>
#else
#include <fcntl.h>
#include <unistd.h>
#endif

namespace lt = libtorrent;
namespace fs = std::filesystem;
using json = nlohmann::json;
using Clock = std::chrono::steady_clock;
thread_local hydra_native_emit output_callback = nullptr;
thread_local hydra_native_poll cancel_callback = nullptr;
thread_local void* callback_context = nullptr;
thread_local std::uint64_t live_limit = std::numeric_limits<std::uint64_t>::max();

struct SessionGuard {
    lt::session& session;
    ~SessionGuard() { auto joined = session.abort(); }
};

lt::tcp::endpoint peer_endpoint(lt::peer_info const& peer) {
#if LIBTORRENT_VERSION_NUM >= 20100
    return peer.remote_endpoint();
#else
    return peer.ip;
#endif
}

bool cancelled() { return cancel_callback(callback_context, &live_limit) != 0; }

constexpr std::size_t max_metadata = 4 * 1024 * 1024;

std::vector<char> read_file(fs::path const& path) {
    std::ifstream stream(path, std::ios::binary);
    if (!stream) throw std::runtime_error("cannot read torrent or resume file");
    stream.seekg(0, std::ios::end);
    auto size = stream.tellg();
    if (size <= 0 || size > std::streamoff(max_metadata)) throw std::runtime_error("invalid or oversized metadata file");
    stream.seekg(0);
    std::vector<char> data(static_cast<std::size_t>(size));
    if (!stream.read(data.data(), size)) throw std::runtime_error("incomplete metadata file");
    return data;
}

std::string hex(std::vector<char> const& bytes) {
    static constexpr char digits[] = "0123456789abcdef";
    std::string out;
    out.reserve(bytes.size() * 2);
    for (unsigned char c : bytes) { out.push_back(digits[c >> 4]); out.push_back(digits[c & 15]); }
    return out;
}

std::vector<char> unhex(std::string const& value) {
    if (value.empty() || value.size() % 2 || value.size() > max_metadata * 2) throw std::runtime_error("invalid metadata length");
    auto digit = [](char c) -> unsigned {
        if (c >= '0' && c <= '9') return c - '0';
        if (c >= 'a' && c <= 'f') return c - 'a' + 10;
        if (c >= 'A' && c <= 'F') return c - 'A' + 10;
        throw std::runtime_error("invalid metadata encoding");
    };
    std::vector<char> out;
    out.reserve(value.size() / 2);
    for (std::size_t i = 0; i < value.size(); i += 2) out.push_back(char((digit(value[i]) << 4) | digit(value[i + 1])));
    return out;
}

std::string identity(lt::torrent_info const& ti) {
    std::ostringstream value;
    auto hashes = ti.info_hashes();
    if (hashes.has_v1()) value << hashes.v1;
    if (hashes.has_v2()) value << hashes.v2;
    return value.str();
}

bool safe_path(std::string const& value) {
    if (value.empty() || value.size() > 4096 || value.front() == '/' || value.find('\\') != std::string::npos) return false;
    for (unsigned char c : value) if (c < 32 || c == 127 || std::string(":<>\"|?*").find(char(c)) != std::string::npos) return false;
    std::istringstream parts(value);
    std::string part;
    while (std::getline(parts, part, '/')) {
        if (part.empty() || part == "." || part == ".." || part.back() == '.' || part.back() == ' ') return false;
        auto stem = part.substr(0, part.find('.'));
        std::transform(stem.begin(), stem.end(), stem.begin(), [](unsigned char c) { return char(std::toupper(c)); });
        if (stem == "CON" || stem == "PRN" || stem == "AUX" || stem == "NUL"
            || (stem.size() == 4 && (stem.substr(0, 3) == "COM" || stem.substr(0, 3) == "LPT") && stem[3] >= '1' && stem[3] <= '9')) return false;
    }
    return value.back() != '/';
}

lt::file_storage const& layout(lt::torrent_info const& ti) {
#if LIBTORRENT_VERSION_NUM >= 20100
    return ti.layout();
#else
    return ti.files();
#endif
}

json files(lt::torrent_info const& ti) {
    json result = json::array();
    std::set<std::string> paths;
    auto const& storage = layout(ti);
    if (storage.num_files() > 4096) throw std::runtime_error("torrent has too many files");
    for (auto i : storage.file_range()) {
        if (storage.pad_file_at(i)) continue;
        if (storage.file_flags(i) & lt::file_storage::flag_symlink) throw std::runtime_error("torrent symlinks are unsupported");
        auto path = storage.file_path(i);
#ifdef _WIN32
        std::replace(path.begin(), path.end(), '\\', '/');
#endif
        auto prefix = ti.name() + "/";
        if (path.rfind(prefix, 0) == 0) path.erase(0, prefix.size());
        auto key = path;
        std::transform(key.begin(), key.end(), key.begin(), [](unsigned char c) { return char(std::tolower(c)); });
        if (!safe_path(path) || !paths.insert(key).second) throw std::runtime_error("torrent contains unsafe or duplicate paths");
        result.push_back({{"index", int(i)}, {"path", path}, {"size", storage.file_size(i)}});
    }
    if (result.empty()) throw std::runtime_error("torrent has no downloadable files");
    return result;
}

lt::settings_pack settings() {
    lt::settings_pack settings;
    settings.set_str(lt::settings_pack::user_agent, "Hydra Torrent/0.1.0");
    settings.set_str(lt::settings_pack::listen_interfaces, "0.0.0.0:0,[::]:0,127.0.0.1:0,[::1]:0");
    settings.set_int(lt::settings_pack::alert_mask, lt::alert_category::error | lt::alert_category::storage | lt::alert_category::status);
    settings.set_bool(lt::settings_pack::enable_upnp, false);
    settings.set_bool(lt::settings_pack::enable_natpmp, false);
    settings.set_bool(lt::settings_pack::enable_lsd, false);
    settings.set_int(lt::settings_pack::connections_limit, 200);
    return settings;
}

void apply_proxy(lt::settings_pack& settings, json const& request) {
    if (!request.contains("proxy") || request.at("proxy").is_null()) return;
    auto const& proxy = request.at("proxy");
    auto kind = proxy.at("kind").get<std::string>();
    if (kind == "none") return;
    auto username = proxy.at("username").is_null() ? std::string() : proxy.at("username").get<std::string>();
    auto password = proxy.at("password").is_null() ? std::string() : proxy.at("password").get<std::string>();
    int type;
    if (kind == "socks5") type = username.empty() ? lt::settings_pack::socks5 : lt::settings_pack::socks5_pw;
    else if (kind == "socks4") type = lt::settings_pack::socks4;
    else if (kind == "http") type = username.empty() ? lt::settings_pack::http : lt::settings_pack::http_pw;
    else throw std::runtime_error("this proxy type is not supported by libtorrent");
    settings.set_int(lt::settings_pack::proxy_type, type);
    settings.set_str(lt::settings_pack::proxy_hostname, proxy.at("host"));
    settings.set_int(lt::settings_pack::proxy_port, proxy.at("port"));
    settings.set_str(lt::settings_pack::proxy_username, username);
    settings.set_str(lt::settings_pack::proxy_password, password);
    settings.set_bool(lt::settings_pack::proxy_hostnames, true);
    settings.set_bool(lt::settings_pack::proxy_peer_connections, true);
    settings.set_bool(lt::settings_pack::proxy_tracker_connections, true);
    settings.set_bool(lt::settings_pack::enable_dht, false);
    settings.set_str(lt::settings_pack::listen_interfaces, "");
}

lt::add_torrent_params inspect(std::string const& input, json const& request) {
    if (input.rfind("magnet:", 0) != 0) throw std::runtime_error("invalid magnet URI");
    auto params = lt::parse_magnet_uri(input);
    // Metadata-only sessions must not download payload before the user selects files.
    params.flags |= lt::torrent_flags::upload_mode;
    params.flags &= ~(lt::torrent_flags::paused | lt::torrent_flags::auto_managed);
    params.file_priorities.assign(4096, lt::dont_download);
    params.save_path = fs::temp_directory_path().u8string();
    auto config = settings();
    apply_proxy(config, request);
    lt::session session(config);
    SessionGuard guard{session};
    auto handle = session.add_torrent(params);
    auto deadline = Clock::now() + std::chrono::seconds(120);
    while (Clock::now() < deadline) {
        if (cancelled()) throw std::runtime_error("torrent metadata resolution cancelled");
        std::vector<lt::alert*> alerts;
        session.pop_alerts(&alerts);
        auto status = handle.status();
        if (status.errc) throw std::runtime_error(status.errc.message());
        if (status.has_metadata) {
            params.ti = std::make_shared<lt::torrent_info>(*handle.torrent_file());
            return params;
        }
        session.wait_for_alert(std::chrono::milliseconds(250));
    }
    throw std::runtime_error("magnet metadata timed out; no reachable peer supplied the file list");
}

void emit(json const& value) {
    auto bytes = value.dump();
    if (output_callback(callback_context, reinterpret_cast<std::uint8_t const*>(bytes.data()), bytes.size())) throw std::runtime_error("native host rejected reply");
}

void assert_no_symlinks(fs::path const& path) {
    fs::path walked;
    for (auto const& part : fs::absolute(path)) {
        walked /= part;
        std::error_code error;
        auto status = fs::symlink_status(walked, error);
        if (error && error != std::errc::no_such_file_or_directory) throw std::runtime_error("cannot inspect destination path");
        if (!error && fs::is_symlink(status)) throw std::runtime_error("torrent destination contains a symlink");
    }
}

void atomic_write(fs::path const& path, std::vector<char> const& bytes) {
    fs::create_directories(path.parent_path());
    auto pending = path;
    pending += ".pending";
    assert_no_symlinks(path);
    assert_no_symlinks(pending);
    {
        std::ofstream output(pending, std::ios::binary | std::ios::trunc);
        if (!output || !output.write(bytes.data(), std::streamsize(bytes.size())) || !output.flush()) throw std::runtime_error("cannot save torrent resume state");
    }
#ifdef _WIN32
    auto file = CreateFileW(pending.c_str(), GENERIC_WRITE, FILE_SHARE_READ, nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr);
    if (file == INVALID_HANDLE_VALUE) throw std::runtime_error("cannot open resume checkpoint for flushing");
    bool flushed = FlushFileBuffers(file) != 0;
    CloseHandle(file);
    if (!flushed || !MoveFileExW(pending.c_str(), path.c_str(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)) throw std::runtime_error("cannot publish resume checkpoint");
#else
    int file = ::open(pending.c_str(), O_RDONLY);
    if (file < 0) throw std::runtime_error("cannot open resume checkpoint for flushing");
    bool flushed = ::fsync(file) == 0;
    ::close(file);
    if (!flushed) throw std::runtime_error("cannot flush resume checkpoint");
    fs::rename(pending, path);
    int directory = ::open(path.parent_path().c_str(), O_RDONLY);
    if (directory < 0) throw std::runtime_error("cannot open checkpoint directory");
    flushed = ::fsync(directory) == 0;
    ::close(directory);
    if (!flushed) throw std::runtime_error("cannot flush checkpoint directory");
#endif
}

void save(lt::session& session, lt::torrent_handle const& handle, fs::path const& path) {
    handle.save_resume_data(lt::torrent_handle::save_info_dict | lt::torrent_handle::flush_disk_cache);
    auto deadline = Clock::now() + std::chrono::seconds(10);
    while (Clock::now() < deadline) {
        std::vector<lt::alert*> alerts;
        session.pop_alerts(&alerts);
        for (auto const* alert : alerts) {
            if (auto const* saved = lt::alert_cast<lt::save_resume_data_alert>(alert)) {
                atomic_write(path, lt::write_resume_data_buf(saved->params));
                return;
            }
            if (auto const* failed = lt::alert_cast<lt::save_resume_data_failed_alert>(alert)) throw std::runtime_error(failed->error.message());
        }
        session.wait_for_alert(std::chrono::milliseconds(100));
    }
    throw std::runtime_error("saving torrent resume state timed out");
}

void download(json const& request) {
    auto const& transfer = request.at("transfer");
    auto const& torrent = transfer.at("metadata");
    auto bytes = unhex(torrent.at("metainfo_hex"));
    auto fresh = lt::load_torrent_buffer(bytes);
    auto description = files(*fresh.ti);
    if (description != transfer.at("files")) throw std::runtime_error("torrent file list does not match its metadata");
    auto chosen_destination = fs::absolute(fs::u8path(request.at("destination").get<std::string>()));
    auto destination = fs::weakly_canonical(chosen_destination.parent_path()) / chosen_destination.filename();
    auto chosen_resume = fs::absolute(fs::u8path(request.at("resume_path").get<std::string>()));
    auto resume = fs::weakly_canonical(chosen_resume.parent_path()) / chosen_resume.filename();
    bool single_file = transfer.value("output", std::string("directory")) == "file";
    if (single_file && description.size() != 1) throw std::runtime_error("single-file output requires exactly one file");
    auto storage_root = single_file ? destination.parent_path() : destination;
    assert_no_symlinks(destination);
    assert_no_symlinks(resume);
    for (auto const& file : description) assert_no_symlinks(single_file ? destination : destination / fs::u8path(file.at("path").get<std::string>()));
    fs::create_directories(storage_root);
    auto params = fresh;
    if (fs::exists(resume)) {
        try {
            auto resumed = lt::read_resume_data(read_file(resume));
            if (resumed.info_hashes != fresh.ti->info_hashes()) throw std::runtime_error("resume state belongs to another torrent");
            params = std::move(resumed);
        } catch (lt::system_error const&) {
            // Invalid fast-resume data falls back to checking every existing piece.
            params = fresh;
        }
    }
    params.ti = fresh.ti;
    params.trackers = fresh.trackers;
    params.tracker_tiers = fresh.tracker_tiers;
    params.url_seeds = fresh.url_seeds;
#if LIBTORRENT_VERSION_NUM < 20100
    params.http_seeds = fresh.http_seeds;
#endif
    params.renamed_files.clear();
    for (auto const& file : description) {
        params.renamed_files[lt::file_index_t(file.at("index").get<int>())] = single_file ? destination.filename().u8string() : file.at("path").get<std::string>();
    }
    // Saved piece bits cannot attest files that changed while Hydra was closed.
    params.have_pieces.clear();
    params.verified_pieces.clear();
    params.unfinished_pieces.clear();
    params.save_path = storage_root.u8string();
    params.flags &= ~(lt::torrent_flags::paused | lt::torrent_flags::auto_managed | lt::torrent_flags::upload_mode | lt::torrent_flags::seed_mode | lt::torrent_flags::no_verify_files);
    params.file_priorities.assign(fresh.ti->num_files(), lt::dont_download);
    std::set<int> wanted;
    if (request.at("files").is_null()) {
        for (auto const& file : description) wanted.insert(file.at("index").get<int>());
    } else {
        for (auto const& index : request.at("files")) wanted.insert(index.get<int>());
    }
    if (wanted.empty()) throw std::runtime_error("select at least one torrent file");
    for (int index : wanted) {
        auto found = std::find_if(description.begin(), description.end(), [index](json const& f) { return f.at("index") == index; });
        if (found == description.end()) throw std::runtime_error("invalid torrent file selection");
        params.file_priorities[std::size_t(index)] = lt::default_priority;
    }
    auto config = settings();
    if (fresh.ti->priv()) config.set_bool(lt::settings_pack::enable_dht, false);
    auto limit = [](std::uint64_t value) { return int(std::min(value, std::uint64_t(std::numeric_limits<int>::max()))); };
    config.set_int(lt::settings_pack::download_rate_limit, limit(request.value("download_limit", std::uint64_t(0))));
    config.set_str(lt::settings_pack::listen_interfaces, torrent.value("listen_interfaces", std::string("0.0.0.0:0,[::]:0,127.0.0.1:0,[::1]:0")));
    config.set_int(lt::settings_pack::upload_rate_limit, limit(torrent.value("upload_limit", std::uint64_t(0))));
    auto seed_seconds = torrent.value("seed_seconds", 0u);
    if (seed_seconds > 86400) throw std::runtime_error("seeding time exceeds 24 hours");
    apply_proxy(config, request);
    lt::session session(config);
    lt::ip_filter classes;
    auto global = 1u << static_cast<std::uint32_t>(lt::session_handle::global_peer_class_id);
    classes.add_rule(lt::make_address("0.0.0.0"), lt::make_address("255.255.255.255"), global);
    classes.add_rule(lt::make_address("::"), lt::make_address("ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff"), global);
    session.set_peer_class_filter(classes);
    auto handle = session.add_torrent(params);
    SessionGuard guard{session};
    auto last_save = Clock::now();
    auto seed_started = Clock::time_point::max();
    json progress;
    std::string network_error;
    json logs = json::array({{{"level", "info"}, {"message", "Torrent transfer started; libtorrent " LIBTORRENT_VERSION}}});
    std::map<std::pair<std::string, std::string>, Clock::time_point> diagnostics;
    auto diagnostic = [&](char const* level, std::string message) {
        auto key = std::make_pair(std::string(level), message);
        auto found = diagnostics.find(key);
        if (found != diagnostics.end() && Clock::now() - found->second < std::chrono::seconds(30)) return;
        if (logs.size() >= 32) return;
        if (diagnostics.size() >= 128) diagnostics.erase(diagnostics.begin());
        diagnostics[key] = Clock::now();
        logs.push_back({{"level", level}, {"message", std::move(message)}});
    };
    diagnostic("debug", "Selected " + std::to_string(wanted.size()) + " files; resume state " + (fs::exists(resume) ? "loaded; disk verification required" : "absent"));
    auto last_diagnostic = Clock::now();
    std::string previous_state;
    bool disk_checked = false;
    bool suspended = false;
    std::uint64_t applied_limit = request.value("download_limit", std::uint64_t(0));
    while (true) {
        bool stopping = cancelled();
        if (live_limit != std::numeric_limits<std::uint64_t>::max()) {
            bool suspend = live_limit == std::numeric_limits<std::uint64_t>::max() - 1;
            if (suspend != suspended) { if (suspend) session.pause(); else session.resume(); suspended = suspend; diagnostic("debug", suspend ? "Traffic suspended by host rate budget" : "Traffic resumed by host rate budget"); }
            if (!suspend && live_limit != applied_limit) {
                lt::settings_pack update;
                update.set_int(lt::settings_pack::download_rate_limit, limit(live_limit));
                session.apply_settings(update);
                applied_limit = live_limit;
                diagnostic("debug", "Download limit changed to " + std::to_string(live_limit) + " bytes/s");
            }
        }
        std::vector<lt::alert*> alerts;
        session.pop_alerts(&alerts);
        for (auto const* alert : alerts) {
            if (lt::alert_cast<lt::torrent_checked_alert>(alert)) { disk_checked = true; diagnostic("debug", "On-disk piece verification finished"); }
            if (auto const* tracker = lt::alert_cast<lt::tracker_error_alert>(alert)) { network_error = tracker->error.message(); diagnostic("debug", "Tracker error: " + network_error); }
            if (auto const* peer = lt::alert_cast<lt::peer_error_alert>(alert)) { network_error = peer->error.message(); diagnostic("debug", "Peer connection error: " + network_error); }
        }
        auto status = handle.status(lt::torrent_handle::query_accurate_download_counters);
        if (status.errc) throw std::runtime_error(status.errc.message());
        bool finished = status.is_finished;
        bool checking = status.state == lt::torrent_status::checking_files || status.state == lt::torrent_status::checking_resume_data;
        if (finished && !checking && seed_started == Clock::time_point::max()) seed_started = Clock::now();
        std::vector<std::int64_t> file_progress;
        handle.file_progress(file_progress, lt::torrent_handle::piece_granularity);
        std::uint64_t verified = 0;
        std::uint64_t total = 0;
        for (auto const& file : description) {
            int index = file.at("index");
            if (wanted.count(index)) {
                verified += std::uint64_t(file_progress.at(std::size_t(index)));
                total += file.at("size").get<std::uint64_t>();
            }
        }
        progress = {{"state", checking ? "checking" : finished ? "seeding" : "downloading"},
            {"done", verified}, {"total", total},
            {"download_rate", status.download_payload_rate}, {"upload_rate", status.upload_payload_rate}, {"peers", status.num_peers}};
        auto state = progress.at("state").get<std::string>();
        if (state != previous_state) { diagnostic("info", "Transfer state: " + state); previous_state = state; }
        if (Clock::now() - last_diagnostic >= std::chrono::seconds(10)) {
            diagnostic("debug", "Verified " + std::to_string(verified) + "/" + std::to_string(total) + " bytes; peers=" + std::to_string(status.num_peers));
            last_diagnostic = Clock::now();
        }
        std::vector<lt::peer_info> peers;
        handle.get_peer_info(peers);
        std::sort(peers.begin(), peers.end(), [](auto const& a, auto const& b) { return peer_endpoint(a) < peer_endpoint(b); });
        json details = json::array();
        for (auto const& peer : peers) {
            if (details.size() >= 64) break;
            auto endpoint = peer_endpoint(peer);
            auto address = endpoint.address().to_string();
            if (endpoint.address().is_v6()) address = "[" + address + "]";
            std::string client;
            for (unsigned char character : peer.client) {
                if (client.size() == 256) break;
                client += character >= 32 && character < 127 ? char(character) : '?';
            }
            details.push_back(json::array({address + ":" + std::to_string(endpoint.port()),
                std::to_string(peer.total_download), std::to_string(peer.total_upload),
                std::to_string(peer.down_speed), std::to_string(peer.up_speed), client}));
        }
        progress["details"] = request.at("transfer").contains("details") && !request.at("transfer").at("details").is_null() ? details : json::array();
        progress["logs"] = logs;
        logs = json::array();
        if (!network_error.empty()) progress["message"] = network_error;
        bool complete = disk_checked && finished && !checking && Clock::now() - seed_started >= std::chrono::seconds(seed_seconds);
        if (stopping || complete) {
            handle.pause();
            save(session, handle, resume);
            progress["state"] = complete ? "complete" : "stopped";
            progress["logs"].push_back({{"level", "info"}, {"message", complete ? "Transfer complete; resume checkpoint saved" : "Transfer stopped; resume checkpoint saved"}});
            emit(progress);
            return;
        }
        emit(progress);
        if (Clock::now() - last_save >= std::chrono::seconds(30)) {
            save(session, handle, resume);
            last_save = Clock::now();
            diagnostic("debug", "Periodic resume checkpoint saved");
        }
        std::this_thread::sleep_for(std::chrono::milliseconds(250));
    }
}

extern "C" HYDRA_NATIVE_EXPORT int32_t hydra_native_v1(
    const uint8_t* method_bytes, size_t method_len,
    const uint8_t* request_bytes, size_t request_len,
    hydra_native_emit output, hydra_native_poll poll, void* context) {
    output_callback = output;
    cancel_callback = poll;
    callback_context = context;
    live_limit = std::numeric_limits<std::uint64_t>::max();
    try {
        if (!method_bytes || !request_bytes || !output || !poll || method_len > 64 || request_len > max_metadata * 2 + 4 * 1024 * 1024) throw std::runtime_error("invalid native call");
        std::string method(reinterpret_cast<char const*>(method_bytes), method_len);
        auto request = json::parse(request_bytes, request_bytes + request_len);
        if (method == "check") {
            emit({{"engine", "libtorrent"}, {"version", LIBTORRENT_VERSION}});
        } else if (method == "inspect") {
            auto params = request.contains("metainfo_hex")
                ? lt::load_torrent_buffer(unhex(request.at("metainfo_hex")))
                : inspect(request.at("magnet").get<std::string>(), request);
            auto bytes = lt::write_torrent_file_buf(params, lt::write_flags::allow_missing_piece_layer);
            if (bytes.size() > max_metadata) throw std::runtime_error("torrent metadata is too large");
            bool single_file = params.ti->num_files() == 1 && layout(*params.ti).file_path(lt::file_index_t(0)).find_first_of("/\\") == std::string::npos;
            emit({{"id", identity(*params.ti)}, {"title", params.ti->name()}, {"tracks", json::array()},
                {"transfer", {{"engine", "torrent-engine"}, {"details", {{"title", "Peer connections"}, {"columns", {"Peer", "Downloaded (bytes)", "Uploaded (bytes)", "Down (bytes/s)", "Up (bytes/s)", "Client"}}}}, {"output", single_file ? "file" : "directory"}, {"metadata", {{"metainfo_hex", hex(bytes)}}}, {"files", files(*params.ti)}, {"notice", "Uploads pieces to peers while downloading. Seeding stops when the download finishes unless enabled in plugin settings."}}}});
        } else if (method == "download") {
            download(request);
        } else { throw std::runtime_error("unknown native method"); }
        return 0;
    } catch (std::exception const& error) {
        auto message = json{{"state", "error"}, {"error", error.what()}}.dump();
        if (output) output(context, reinterpret_cast<uint8_t const*>(message.data()), message.size());
        return 1;
    } catch (...) {
        return 1;
    }
}
