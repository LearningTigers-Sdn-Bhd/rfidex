// RfiDex gate helper. Mirrors the vendor's EC_rfidReader_library_gate demo
// (Form1.cs): RDR_Open with a connection string, LibraryGate_FetchRecords with
// flags 0x02 then 0x01, RDR_SetLibraryGateAlarm. It speaks RfiDex's parent/child
// protocol (crates/rfidex-hardware/src/wire.rs): a 4-byte big-endian length then
// JSON, the session token first and alone.
//
// rfidclib_reader.dll is loaded by reflection so this program builds without
// the vendor's file. Put rfidclib_reader.dll in the RfiDex install folder.
using System;
using System.Collections;
using System.Collections.Generic;
using System.IO;
using System.Net.Sockets;
using System.Reflection;
using System.Text;
using System.Web.Script.Serialization;

namespace RfiDex.GateHost
{
    static class Program
    {
        const string Switch = "--rfid-device-host";
        const string DefaultModel = "D5300";
        // ponytail: no-record replies are indistinguishable from a dead link
        // (the demo ignores every non-zero code), so liveness is asked of the
        // reader itself once codes persist this long.
        const int QuietCheckMs = 5000;

        static readonly JavaScriptSerializer Json = new JavaScriptSerializer();

        static int Main(string[] args)
        {
            if (args.Length != 3 || args[0] != Switch) return 2;
            int port;
            if (!int.TryParse(args[1], out port) || port <= 0) return 2;
            try
            {
                using (var client = new TcpClient())
                {
                    client.NoDelay = true;
                    client.Connect("127.0.0.1", port);
                    var stream = client.GetStream();
                    WriteFrame(stream, Json.Serialize(args[2]));
                    Serve(stream);
                }
                return 0;
            }
            catch (Exception)
            {
                return 3;
            }
        }

        static void Serve(NetworkStream stream)
        {
            Reader reader = null;
            try
            {
                while (true)
                {
                    string frame = ReadFrame(stream);
                    if (frame == null) return;
                    var request = (Dictionary<string, object>)Json.DeserializeObject(frame);
                    long id = Convert.ToInt64(request["id"]);
                    var operation = (Dictionary<string, object>)request["operation"];
                    string result;
                    try
                    {
                        result = "{\"Ok\":" + Dispatch(ref reader, operation) + "}";
                    }
                    catch (SdkError e)
                    {
                        result = "{\"Err\":{\"sdk\":" + e.Code + "}}";
                    }
                    catch (WireFailure e)
                    {
                        result = "{\"Err\":\"" + e.Name + "\"}";
                    }
                    WriteFrame(stream, "{\"id\":" + id + ",\"result\":" + result + "}");
                }
            }
            finally
            {
                if (reader != null) reader.Close();
            }
        }

        static string Dispatch(ref Reader reader, Dictionary<string, object> operation)
        {
            switch ((string)operation["op"])
            {
                case "open":
                    if (reader != null) { reader.Close(); reader = null; }
                    reader = Reader.Open((Dictionary<string, object>)operation["config"]);
                    return "{\"kind\":\"unit\"}";
                case "close":
                    if (reader != null) { reader.Close(); reader = null; }
                    return "{\"kind\":\"unit\"}";
                case "info":
                    return Need(reader).Info();
                case "library_records":
                    return Need(reader).Records(Convert.ToByte(operation["flag"]));
                case "library_alarm":
                    Need(reader).Alarm(Convert.ToByte(operation["mode"]));
                    return "{\"kind\":\"unit\"}";
                default:
                    // Desk operations (inventory, read, write) belong to the
                    // ECRFID helper; a gate does not ask for them here.
                    throw new WireFailure("unsupported");
            }
        }

        static Reader Need(Reader reader)
        {
            if (reader == null) throw new WireFailure("disconnected");
            return reader;
        }

        // ---- framing -----------------------------------------------------

        static string ReadFrame(NetworkStream s)
        {
            var head = new byte[4];
            if (!ReadExact(s, head, 4)) return null;
            int len = (head[0] << 24) | (head[1] << 16) | (head[2] << 8) | head[3];
            if (len < 0 || len > 65536) throw new InvalidDataException();
            var body = new byte[len];
            if (!ReadExact(s, body, len)) return null;
            return Encoding.UTF8.GetString(body);
        }

        static bool ReadExact(NetworkStream s, byte[] buf, int n)
        {
            int got = 0;
            while (got < n)
            {
                int r = s.Read(buf, got, n - got);
                if (r <= 0) return false;
                got += r;
            }
            return true;
        }

        static void WriteFrame(NetworkStream s, string json)
        {
            var body = Encoding.UTF8.GetBytes(json);
            var head = new byte[]
            {
                (byte)(body.Length >> 24), (byte)(body.Length >> 16),
                (byte)(body.Length >> 8), (byte)body.Length
            };
            s.Write(head, 0, 4);
            s.Write(body, 0, body.Length);
            s.Flush();
        }

        // ---- vendor reader ----------------------------------------------

        sealed class Reader
        {
            readonly object lib;
            readonly Type type;
            readonly string model;
            readonly bool reverseUid;
            DateTime quietSince = DateTime.MinValue;

            Reader(object lib, string model)
            {
                this.lib = lib;
                type = lib.GetType();
                this.model = model;
                reverseUid = Environment.GetEnvironmentVariable("RFIDEX_GATE_UID_REVERSE") == "1";
            }

            public static Reader Open(Dictionary<string, object> config)
            {
                var connection = (Dictionary<string, object>)config["connection"];
                if ((string)connection["kind"] != "net") throw new WireFailure("unsupported");
                string model = ((string)connection["model"] ?? "").Trim();
                if (model.Length == 0) model = DefaultModel;
                string address = (string)connection["address"];
                int colon = address.LastIndexOf(':');
                string ip = address.Substring(0, colon);
                string port = address.Substring(colon + 1);
                string dll = (string)config["dll_path"];

                var lib = LoadLibrary(dll);
                var reader = new Reader(lib, model);
                // The demo's own preparation call before opening.
                try { reader.Call("CRC16", 0xAA, new byte[] { 0x00 }); } catch (Exception) { }
                string connstr = "RDType=" + model + ";CommType=NET;RemoteIP=" + ip +
                                 ";RemotePort=" + port;
                int rc = (int)reader.Call("RDR_Open", connstr);
                if (rc != 0) throw new SdkError(rc);
                return reader;
            }

            static object LoadLibrary(string configured)
            {
                string here = AppDomain.CurrentDomain.BaseDirectory;
                var candidates = new List<string>();
                if (!string.IsNullOrEmpty(configured))
                {
                    candidates.Add(Path.Combine(Path.GetDirectoryName(configured) ?? "", "rfidclib_reader.dll"));
                }
                candidates.Add(Path.Combine(here, "rfidclib_reader.dll"));
                candidates.Add(Path.Combine(Path.GetFullPath(Path.Combine(here, "..")), "rfidclib_reader.dll"));
                foreach (string path in candidates)
                {
                    if (!File.Exists(path)) continue;
                    var asm = Assembly.LoadFrom(path);
                    return Activator.CreateInstance(asm.GetType("RFIDCLIB.rfidclib_reader", true));
                }
                throw new WireFailure("disconnected");
            }

            object Call(string name, params object[] args)
            {
                var method = type.GetMethod(name);
                if (method == null) throw new WireFailure("unsupported");
                try { return method.Invoke(lib, args); }
                catch (TargetInvocationException e) { throw e.InnerException ?? e; }
            }

            public string Info()
            {
                object[] args = { "" };
                int rc = (int)Call("RDR_GetReaderInfor", args);
                if (rc != 0) throw new SdkError(rc);
                string[] parts = ((string)args[0] ?? "").Split('|');
                string firmware = parts.Length > 0 ? parts[0] : null;
                return "{\"kind\":\"info\",\"model\":" + Json.Serialize(model) +
                       ",\"firmware\":" + Json.Serialize(firmware) + ",\"raw\":[]}";
            }

            // One fetch, one pass at most, as the demo's Inventory loop does.
            public string Records(byte flag)
            {
                object[] args =
                {
                    flag, new byte[8], new byte[4], (byte)0, (byte)0,
                    new byte[6], new byte[4], new byte[4]
                };
                int rc = (int)Call("LibraryGate_FetchRecords", args);
                if (rc != 0)
                {
                    // The demo ignores non-zero codes. Only when they persist
                    // is the reader asked whether it is still there.
                    if (quietSince == DateTime.MinValue) quietSince = DateTime.UtcNow;
                    else if ((DateTime.UtcNow - quietSince).TotalMilliseconds > QuietCheckMs)
                    {
                        quietSince = DateTime.UtcNow;
                        Info();
                    }
                    return "{\"kind\":\"passes\",\"passes\":[]}";
                }
                quietSince = DateTime.MinValue;
                var uid = args[1] as byte[];
                if (uid == null || uid.Length != 8)
                    return "{\"kind\":\"passes\",\"passes\":[]}";
                if (reverseUid) Array.Reverse(uid);
                var time = args[5] as byte[] ?? new byte[6];
                return "{\"kind\":\"passes\",\"passes\":[{\"uid\":" + Bytes(uid) +
                       ",\"direction\":" + (byte)args[4] +
                       ",\"alarm\":" + (byte)args[3] +
                       ",\"time\":" + Bytes(time) + "}]}";
            }

            public void Alarm(byte mode)
            {
                int rc = (int)Call("RDR_SetLibraryGateAlarm", mode);
                if (rc != 0) throw new SdkError(rc);
            }

            public void Close()
            {
                try { Call("RDR_Close"); } catch (Exception) { }
            }

            static string Bytes(byte[] b)
            {
                var sb = new StringBuilder("[");
                for (int i = 0; i < b.Length; i++)
                {
                    if (i > 0) sb.Append(',');
                    sb.Append(b[i]);
                }
                return sb.Append(']').ToString();
            }
        }

        sealed class SdkError : Exception
        {
            public readonly int Code;
            public SdkError(int code) { Code = code; }
        }

        sealed class WireFailure : Exception
        {
            public readonly string Name;
            public WireFailure(string name) { Name = name; }
        }
    }
}
